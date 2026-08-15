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

use crate::{admission::ExecLanes, config::sim_now_ns, fills, run::Run, source::Rivers};

/// Wall floor under the converted sweep interval. Under an accelerated clock
/// `wall_duration` shrinks linearly while the per-pass fixed cost (checkpoint
/// restore, two lock round-trips) does not, so an unfloored sweep at
/// `speed = 100` is a 1 ms hot loop. The floor costs sim-time resolution the
/// gate does not need and buys a cost that stays bounded in wall time.
pub(crate) const MIN_SWEEP_WALL: Duration = Duration::from_millis(5);

pub(crate) struct FillSweep {
    pub(crate) run: Arc<Run>,
    pub(crate) rivers: Arc<Rivers>,
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
        let mut last_swept_ns = sim_now_ns(sim);
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
            // Sampled ONCE for the pass, so every order is judged against the
            // same instant no matter how long the walks take.
            let to_ns = sim_now_ns(sim);
            let scans = { sweep.run.engine.lock().await.pending_scans() };
            let mut groups: HashMap<String, Vec<_>> = HashMap::new();
            for scan in scans {
                groups
                    .entry(scan.symbol.to_string())
                    .or_default()
                    .push(scan);
            }
            let mut results = Vec::new();
            for (symbol, scans) in groups {
                let rivers = Arc::clone(&sweep.rivers);
                let scans_for_walk = scans.clone();
                let walked = tokio::task::spawn_blocking(move || {
                    fills::scan_triggers(&symbol, &scans_for_walk, to_ns, &rivers)
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
            let symbols = { sweep.run.engine.lock().await.futures_mark_symbols() };
            let settlements: Vec<_> = symbols
                .iter()
                .filter_map(|symbol| {
                    sweep
                        .rivers
                        .profiles()
                        .get(symbol)
                        .and_then(|profile| profile.calendar.as_ref())
                        .map(|calendar| {
                            (symbol, calendar.settlement_instants(last_swept_ns, to_ns))
                        })
                })
                .flat_map(|(symbol, instants)| {
                    instants
                        .into_iter()
                        .map(move |instant| (std::sync::Arc::clone(symbol), instant))
                })
                .collect();
            let rivers = Arc::clone(&sweep.rivers);
            let reads = run_blocking(move || read_marks(&symbols, &settlements, to_ns, &rivers));
            let reads = tokio::select! {
                reads = reads => reads.flatten(),
                _ = completion.changed() => break,
            };
            last_swept_ns = frontier_after(last_swept_ns, to_ns, reads.is_some());
            let Some((marks, settlement_marks)) = reads else {
                // The reading task died, or one of this span's settlement
                // instants had no readable price. Abandoning the whole pass is
                // the only safe response either way: the scan results are
                // re-derivable (the engine still holds every pending scan at
                // its unadvanced `from_ns`), while the settlement instants this
                // interval crossed exist nowhere but in the span
                // `last_swept_ns..to_ns`.
                continue;
            };
            let mut engine = sweep.run.engine.lock().await;
            let (events, emitted, originated) =
                apply_engine_pass(&mut engine, &results, settlement_marks, &marks, to_ns);
            let shape = engine.book_shape();
            drop(engine);
            if events.is_empty() {
                continue;
            }
            deliver(&sweep.run, &shape, &events, emitted, originated, to_ns);
        }
    })
}

type MarkReads = (
    Vec<(mogwai_protocol::Symbol, rust_decimal::Decimal)>,
    Vec<(mogwai_protocol::Symbol, u64, rust_decimal::Decimal)>,
);

/// Every tape price one sweep pass needs: the futures marks at `to_ns` and the
/// price at each settlement instant the pass crossed.
///
/// Both are EXACT-INSTANT last-print reads rather than `MarketReadingCache`
/// lookups. That cache buckets by fill-sweep interval, which is a defensible
/// coarseness for a volatility band and is not one for a mark price: unrealized
/// P and L and the margin evaluation that follows it would freeze for every pass
/// sharing a bucket, which under an accelerated clock is several of them.
///
/// The two halves fail DIFFERENTLY, and that asymmetry is the point. An
/// unreadable ordinary mark costs one pass of unrealized P and L freshness and
/// is asked again five milliseconds later, so it is dropped. An unreadable
/// SETTLEMENT price cannot be asked again: `last_swept_ns` is about to move past
/// its instant and nothing looks further back, so the whole read is refused and
/// the caller leaves the watermark where it stands. A `filter_map` here - which
/// is what this was - loses that instant permanently and silently, the same
/// defect shape the round-1 error path had, arriving through a lookup that
/// legitimately returns nothing instead of through a panic.
fn read_marks(
    symbols: &[mogwai_protocol::Symbol],
    settlements: &[(mogwai_protocol::Symbol, u64)],
    to_ns: u64,
    rivers: &Rivers,
) -> Option<MarkReads> {
    let marks: Vec<_> = symbols
        .iter()
        .filter_map(|symbol| {
            fills::read_last(symbol, to_ns, rivers).map(|px| (std::sync::Arc::clone(symbol), px))
        })
        .collect();
    let settlement_marks: Option<Vec<_>> = settlements
        .iter()
        .map(|(symbol, instant)| {
            fills::read_last(symbol, *instant, rivers)
                .map(|px| (std::sync::Arc::clone(symbol), *instant, px))
        })
        .collect();
    Some((marks, settlement_marks?))
}

/// Where the settlement frontier stands after a pass.
///
/// `last_swept_ns` is a WATERMARK, and the only record that a span is still
/// owed settlement: the next pass asks the calendar for the instants inside
/// `last_swept_ns..to_ns` and nothing ever looks further back than that. So it
/// may only move over a span whose settlement prices were actually read.
/// Advancing it on a failed reading pass - which is what treating the failure
/// as an empty result does - retires every settlement instant the span crossed
/// without anyone having priced them, permanently and silently.
///
/// `read` is therefore the success of BOTH halves: the reading task surviving,
/// and every settlement instant in the span having yielded a price. A partial
/// settlement answer never reaches here, because `read_marks` collects its
/// settlement prices into an `Option` rather than filtering the unreadable ones
/// away. That is the load-bearing part: two rounds of this bug hunt have now
/// advanced a frontier past work nobody did, first through an error path and
/// then through a lookup that legitimately returned nothing, so the guard sits
/// where neither door can bypass it.
fn frontier_after(last_swept_ns: u64, to_ns: u64, read: bool) -> u64 {
    if read { to_ns } else { last_swept_ns }
}

async fn run_blocking<F, T>(work: F) -> Option<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| {
            tracing::error!(%error, "fill sweep market-reading task failed");
        })
        .ok()
}

fn apply_engine_pass(
    engine: &mut mogwai_engine::Engine,
    results: &[ScanResult],
    settlement_marks: Vec<(mogwai_protocol::Symbol, u64, rust_decimal::Decimal)>,
    marks: &[(mogwai_protocol::Symbol, rust_decimal::Decimal)],
    to_ns: u64,
) -> (Vec<ServerMessage>, usize, usize) {
    let (mut events, emitted) = engine.apply_scans(results, to_ns);
    let mut originated = 0;
    for (symbol, instant, px) in settlement_marks {
        let settled = engine.settle(&[(symbol, px)], instant);
        originated += settled.originated_orders;
        events.extend(settled.events);
    }
    let marked = engine.mark(marks, to_ns);
    originated += marked.originated_orders;
    events.extend(marked.events);
    // EXACTLY one `AccountState` per pass, and it is the LAST one: scans,
    // every settlement and the mark each snapshot, and every snapshot but the
    // final one reports a stale `mark_px` and `unrealized_pnl`. Dropping the
    // earlier ones unconditionally is what makes the invariant hold on a pass
    // where a settlement snapshotted and the mark did not move.
    let last_state = events
        .iter()
        .rposition(|event| matches!(event, ServerMessage::AccountState(_)));
    if let Some(last_state) = last_state {
        let mut index = 0;
        events.retain(|event| {
            let keep = !matches!(event, ServerMessage::AccountState(_)) || index == last_state;
            index += 1;
            keep
        });
    }
    (events, emitted, originated)
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
    originated: usize,
    ts: u64,
) {
    let subject = (!events.is_empty()).then_some(AdmissionSubject::Frame);
    let mut closed = Vec::new();
    for (id, lane) in run.bound_lanes() {
        let Some(reservation) = lane.reserve_swept(shape, emitted, originated) else {
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

#[cfg(test)]
mod tests {
    use super::*;
    use mogwai_engine::{BreachAction, EngineConfig, MarginPolicy, MarketReading};
    use mogwai_protocol::{
        AccountId, ClientMessage, Hit, InstrumentClass, InstrumentDef, OrderType, Side,
        SubmitOrder, TimeInForce, WireAssetClass,
    };
    use rust_decimal::Decimal;

    #[tokio::test(flavor = "current_thread")]
    async fn blocking_market_walk_does_not_occupy_the_runtime_worker() {
        let runtime_thread = std::thread::current().id();
        let walk = run_blocking(move || {
            std::thread::sleep(Duration::from_millis(50));
            std::thread::current().id()
        });
        tokio::pin!(walk);
        tokio::select! {
            biased;
            () = tokio::time::sleep(Duration::from_millis(5)) => {}
            _ = &mut walk => panic!("blocking walk occupied the runtime worker"),
        };
        let walked_thread = walk.await.unwrap();
        assert_ne!(walked_thread, runtime_thread);
    }

    #[test]
    fn an_unread_pass_leaves_its_settlement_span_owed() {
        let calendar = mogwai_data::SessionCalendar {
            utc_offset_minutes: 0,
            // Wraps the week, so every minute but the epoch-week's first is
            // open and the settlement crossing below cannot be a closure.
            open_windows: vec![mogwai_data::WeeklyWindow {
                start_minute: 1,
                end_minute: 0,
            }],
            settlement_minute_of_day: Some(960),
        };
        calendar.validate().expect("a valid always-open calendar");
        let start = 0u64;
        let week_ns = 7 * 86_400_000_000_000u64;
        let instant = calendar.settlement_instants(start, week_ns)[0];
        // The pass that crossed the settlement and failed to read a price for
        // it, then the pass after it.
        let crossed_to = instant + 60_000_000_000;
        let next_to = crossed_to + 60_000_000_000;

        let held = frontier_after(start, crossed_to, false);
        assert_eq!(held, start, "an unread pass may not move the watermark");
        assert!(
            calendar
                .settlement_instants(held, next_to)
                .contains(&instant),
            "the held watermark must leave the settlement to the next pass"
        );
        // Stated so the assertion above cannot be read as vacuous: the span is
        // the ONLY record, so an advanced watermark loses the instant outright.
        assert!(
            !calendar
                .settlement_instants(crossed_to, next_to)
                .contains(&instant)
        );
        assert_eq!(
            frontier_after(start, crossed_to, true),
            crossed_to,
            "a pass that did read its prices advances normally"
        );
    }

    /// The settlement half of the frontier guard, at the layer that decides it.
    ///
    /// An unreadable ORDINARY mark is dropped and the pass proceeds; an
    /// unreadable SETTLEMENT price refuses the whole read, which is what makes
    /// `frontier_after` leave the span owed. Before this, `read_marks` filtered
    /// both alike, so a settlement instant whose price could not be read was
    /// retired by a watermark that then never looked back at it.
    #[test]
    fn an_unreadable_settlement_price_refuses_the_whole_read() {
        let profiles = crate::fills::test_rivers();
        let readable = crate::source::TAPE_ORIGIN_NS + 86_400_000_000_000;
        let known: mogwai_protocol::Symbol = "BTCUSDT".into();
        let unknown: mogwai_protocol::Symbol = "NOT-A-SYMBOL".into();

        let (marks, settlement_marks) = read_marks(
            std::slice::from_ref(&known),
            &[(std::sync::Arc::clone(&known), readable)],
            readable,
            &profiles,
        )
        .expect("a readable pair answers");
        assert_eq!(marks.len(), 1);
        assert_eq!(settlement_marks.len(), 1);

        let (marks, settlement_marks) = read_marks(
            &[
                std::sync::Arc::clone(&known),
                std::sync::Arc::clone(&unknown),
            ],
            &[(std::sync::Arc::clone(&known), readable)],
            readable,
            &profiles,
        )
        .expect("an unreadable ordinary mark is dropped, not fatal");
        assert_eq!(marks.len(), 1);
        assert_eq!(settlement_marks.len(), 1);

        assert!(
            read_marks(
                std::slice::from_ref(&known),
                &[
                    (std::sync::Arc::clone(&known), readable),
                    (unknown, readable)
                ],
                readable,
                &profiles,
            )
            .is_none(),
            "one unreadable settlement price refuses the pass"
        );
    }

    fn engine_with_position() -> mogwai_engine::Engine {
        let def = InstrumentDef {
            symbol: "MNQ".into(),
            class: InstrumentClass::Future {
                underlying: "NQ".into(),
                settlement_currency: "USD".into(),
                multiplier: Decimal::from(2),
                asset_class: WireAssetClass::Index,
            },
            price_precision: 2,
            size_precision: 0,
            price_increment: Decimal::new(25, 2),
            size_increment: Decimal::ONE,
        };
        let mut engine = mogwai_engine::Engine::build(EngineConfig {
            account_id: AccountId::parse("TEST-001").unwrap(),
            instruments: vec![def],
            balances: HashMap::from([("USD".into(), Decimal::from(10_000))]),
            fill_seed: 7,
        });
        engine.set_margin_policy(
            "MNQ".into(),
            MarginPolicy {
                initial_per_contract: Decimal::from(2_000),
                maintenance_per_contract: Decimal::from(1_800),
                breach_action: BreachAction::Refuse,
            },
        );
        let order = SubmitOrder {
            client_order_id: "OPEN".into(),
            symbol: "MNQ".into(),
            position_id: None,
            side: Side::Buy,
            order_type: OrderType::Market,
            quantity: Decimal::ONE,
            price: Some(Decimal::from(21_000)),
            trigger_price: None,
            reduce_only: false,
            post_only: false,
            time_in_force: TimeInForce::Gtc,
        };
        engine.process_with_market(
            ClientMessage::SubmitOrder(order),
            1,
            Some(MarketReading {
                last_px: Decimal::from(21_000),
                ts_ns: 1,
                band_ticks: 0,
            }),
        );
        engine
    }

    #[test]
    fn a_futures_run_marks_with_no_resting_orders() {
        let mut engine = engine_with_position();
        let (events, emitted, _) = apply_engine_pass(
            &mut engine,
            &[],
            Vec::new(),
            &[("MNQ".into(), Decimal::from(21_001))],
            2,
        );
        assert_eq!(emitted, 0);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ServerMessage::AccountState(_)))
        );
    }

    #[test]
    fn a_pass_emits_exactly_one_account_state_after_marking() {
        let mut engine = engine_with_position();
        let (events, _, _) = apply_engine_pass(
            &mut engine,
            &[],
            Vec::new(),
            &[("MNQ".into(), Decimal::from(21_001))],
            2,
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ServerMessage::AccountState(_)))
                .count(),
            1
        );
    }

    #[test]
    fn two_settlements_crossed_by_one_pass_book_at_their_own_prices() {
        let mut engine = engine_with_position();
        let (events, _, _) = apply_engine_pass(
            &mut engine,
            &[],
            vec![
                ("MNQ".into(), 2, Decimal::from(21_001)),
                ("MNQ".into(), 3, Decimal::from(21_003)),
            ],
            &[("MNQ".into(), Decimal::from(21_003))],
            3,
        );
        let state = events
            .iter()
            .rev()
            .find_map(|event| match event {
                ServerMessage::AccountState(state) => Some(state),
                _ => None,
            })
            .unwrap();
        assert_eq!(state.balances[0].total, Decimal::from(10_006));
        assert_eq!(state.positions[0].avg_px, Decimal::from(21_003));
    }

    #[test]
    fn a_resting_order_survives_a_closure_and_fills_after_the_reopen() {
        let mut engine = engine_with_position();
        let order = SubmitOrder {
            client_order_id: "REST".into(),
            symbol: "MNQ".into(),
            position_id: None,
            side: Side::Sell,
            order_type: OrderType::Limit,
            quantity: Decimal::ONE,
            price: Some(Decimal::from(21_100)),
            trigger_price: None,
            reduce_only: true,
            post_only: false,
            time_in_force: TimeInForce::Gtc,
        };
        engine.process(ClientMessage::SubmitOrder(order), 2);
        let scan = engine
            .pending_scans()
            .into_iter()
            .find(|scan| scan.client_order_id == "REST")
            .unwrap();
        let closed = ScanResult {
            client_order_id: scan.client_order_id.clone(),
            from_ns: scan.from_ns,
            revision: scan.revision,
            hit: None,
            scanned_to_ns: 3,
        };
        let (closed_events, _) = engine.apply_scans(&[closed], 3);
        assert!(
            !closed_events
                .iter()
                .any(|event| matches!(event, ServerMessage::OrderFilled(_)))
        );
        // SURVIVES, which is the half the fill assertion below cannot see: a
        // closure that cancelled the order would also produce no fill here.
        assert!(
            engine
                .open_orders()
                .iter()
                .any(|order| order.submit.client_order_id == "REST"),
            "a resting order must persist across a closure, not be cancelled by it"
        );
        let scan = engine
            .pending_scans()
            .into_iter()
            .find(|scan| scan.client_order_id == "REST")
            .unwrap();
        let reopened = ScanResult {
            client_order_id: scan.client_order_id,
            from_ns: scan.from_ns,
            revision: scan.revision,
            hit: Some(Hit {
                ts_ns: 4,
                px: Decimal::from(21_100),
            }),
            scanned_to_ns: 4,
        };
        let (events, _) = engine.apply_scans(&[reopened], 4);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ServerMessage::OrderFilled(_)))
        );
    }

    #[test]
    fn the_mark_freezes_across_a_closure() {
        let mut engine = engine_with_position();
        apply_engine_pass(
            &mut engine,
            &[],
            Vec::new(),
            &[("MNQ".into(), Decimal::from(21_001))],
            2,
        );
        let before = engine.account_snapshot(2).positions[0].clone();
        assert_eq!(before.mark_px, Decimal::from(21_001));

        // A closure is an EMPTY mark set: there is no tape inside it, so the
        // sweeper reads no price and hands the engine nothing. Re-passing the
        // same price instead would assert nothing at all - it holds whether or
        // not the mark is frozen.
        for ts in 3..6 {
            let (events, _, _) = apply_engine_pass(&mut engine, &[], Vec::new(), &[], ts);
            assert!(events.is_empty(), "a closed pass moves nothing: {events:?}");
        }
        let after = engine.account_snapshot(6).positions[0].clone();
        assert_eq!(after.mark_px, before.mark_px);
        assert_eq!(after.unrealized_pnl, before.unrealized_pnl);

        // And the freeze thaws: the first mark after the reopen moves again.
        apply_engine_pass(
            &mut engine,
            &[],
            Vec::new(),
            &[("MNQ".into(), Decimal::from(21_005))],
            7,
        );
        assert_eq!(
            engine.account_snapshot(7).positions[0].mark_px,
            Decimal::from(21_005)
        );
    }
}
