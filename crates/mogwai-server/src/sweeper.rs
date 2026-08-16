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
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

use mogwai_engine::{PendingScan, ScanResult};
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
        let mut completion = sweep.run.completion();
        // EARLIEST-DEADLINE SCHEDULE, one task. The sweep interval is SIMULATED
        // milliseconds, so it converts through a clock before it becomes a wall
        // sleep - and since `/ws?speed=` landed, the boats on one venue no
        // longer share a speed, so there is no single conversion. Each boat
        // therefore carries its own next-due instant and the task sleeps to the
        // EARLIEST of them. Deliberately not one task per boat: N tasks contend
        // the single engine lock and multiply the completion fan-out, to buy
        // cadence granularity that settlement correctness does not rest on
        // (per-boat `to_ns` and per-boat `last_swept_ns` already carry that).
        //
        // Keyed by `BoatKey`, not by the `Arc`'s address: an address is reused
        // after the last ticket drops a boat, so a newly seated boat could
        // inherit a departed one's due instant.
        let mut next_due: HashMap<crate::boatyard::BoatKey, (Arc<crate::boatyard::Boat>, Instant)> =
            HashMap::new();
        // Labelled because the per-boat body below breaks on completion: an
        // unlabelled break there would leave only the boat loop and the
        // sweeper would keep walking a completed run forever.
        'passes: loop {
            // Re-derived every pass: boats appear and leave under this task. A
            // boat seated mid-pass is due immediately on the next one, which is
            // the latency it had under the old shared cadence too.
            let now = Instant::now();
            let boats = sweep.run.boatyard.boats();
            let seated: HashSet<_> = boats.iter().map(|boat| boat.key()).collect();
            next_due.retain(|key, _| seated.contains(key));
            for boat in boats {
                next_due.entry(boat.key()).or_insert((boat, now));
            }
            // A boatless venue has nothing to sweep, but the loop must still
            // tick to observe completion, so it falls back to the venue clock's
            // conversion.
            let venue_wall = sweep
                .run
                .sim
                .wall_duration(crate::config::sim_duration_from_millis(sweep.interval_ms))
                .max(MIN_SWEEP_WALL);
            let deadline = next_due
                .values()
                .map(|(_, due)| *due)
                .min()
                .unwrap_or(now + venue_wall);
            // A completed run stops walking the tape at once rather than one
            // interval late: the completion sequence is already announcing
            // itself on every socket, and a fill booked after that would be a
            // fill nobody is listening for.
            tokio::select! {
                () = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {}
                _ = completion.changed() => break 'passes,
            }
            // One acquisition PER PASSENGER, so each ledger's scans and mark
            // symbols come from one consistent read of it.
            //
            // Gathered across every passenger BEFORE the walk, deliberately: the
            // tape walk is the expensive part of a pass and is a property of the
            // river, not of any ledger, so N passengers resting orders on one
            // symbol still walk it ONCE. Results are grouped back per passenger
            // afterwards.
            let seated = sweep.run.passengers();
            let mut scans: Vec<(usize, PendingScan)> = Vec::new();
            let mut mark_symbols = Vec::new();
            for (index, passenger) in seated.iter().enumerate() {
                let engine = passenger.engine.lock().await;
                scans.extend(engine.pending_scans().into_iter().map(|scan| (index, scan)));
                // Valuation symbols, not just the margin ones: a policed
                // account holding a spot asset needs that pair priced to state
                // its equity at all, and nothing else in the pass would ask for
                // it.
                mark_symbols.extend(engine.valuation_symbols());
            }
            mark_symbols.sort();
            mark_symbols.dedup();
            // Only the boats whose own converted interval has elapsed, each
            // re-armed on ITS clock and floored at `MIN_SWEEP_WALL` as before.
            let now = Instant::now();
            let due_boats: Vec<_> = next_due
                .values_mut()
                .filter(|(_, due)| *due <= now)
                .map(|(boat, due)| {
                    *due = now
                        + boat
                            .sim
                            .wall_duration(crate::config::sim_duration_from_millis(
                                sweep.interval_ms,
                            ))
                            .max(MIN_SWEEP_WALL);
                    Arc::clone(boat)
                })
                .collect();
            for boat in due_boats {
                let symbol = boat.symbol().to_owned();
                let to_ns = sim_now_ns(boat.sim);
                // Every passenger's scans on this river, in ONE list, because the
                // walk is a property of the water. The passenger index rides
                // along so each result can be applied to the ledger it came
                // from.
                let boat_scans: Vec<(usize, PendingScan)> = scans
                    .iter()
                    .filter(|(_, scan)| scan.symbol.as_ref() == symbol)
                    .cloned()
                    .collect();
                let mut results: Vec<Vec<ScanResult>> = vec![Vec::new(); seated.len()];
                let rivers = Arc::clone(&sweep.rivers);
                let scans_for_walk: Vec<PendingScan> =
                    boat_scans.iter().map(|(_, scan)| scan.clone()).collect();
                let walk_symbol = symbol.clone();
                let walked = tokio::task::spawn_blocking(move || {
                    fills::scan_triggers(&walk_symbol, &scans_for_walk, to_ns, &rivers)
                })
                .await
                .ok()
                .flatten();
                // A `None` walk (the positioning seek could not reach the
                // earliest frontier) yields no result at all for the symbol, so
                // nothing advances: an unreachable span is not a span
                // nothing triggered in.
                if let Some(walk) = walked {
                    for ((owner, scan), hit) in boat_scans.into_iter().zip(walk.hits) {
                        results[owner].push(ScanResult {
                            client_order_id: scan.client_order_id,
                            from_ns: scan.from_ns,
                            revision: scan.revision,
                            hit,
                            scanned_to_ns: walk.reached_ns,
                        });
                    }
                }
                let last_swept_ns = boat
                    .last_swept_ns
                    .load(std::sync::atomic::Ordering::Acquire);
                let symbols: Vec<_> = mark_symbols
                    .iter()
                    .filter(|candidate| candidate.as_ref() == symbol)
                    .cloned()
                    .collect();
                // Whether THIS boat's river crossed a session close in the span
                // just swept, which is what expires a Day order. Asked of the
                // calendar rather than derived from a clock: the venue already
                // knows when a session ends, and only an instrument that HAS a
                // calendar has a day to end.
                let session_closed = sweep
                    .rivers
                    .resolve_profile(&symbol)
                    .ok()
                    .and_then(|profile| profile.calendar.clone())
                    .filter(|calendar| calendar.is_open(last_swept_ns) && !calendar.is_open(to_ns))
                    .map(|_| mogwai_protocol::Symbol::from(symbol.as_str()));
                let settlements: Vec<_> = symbols
                    .iter()
                    .filter_map(|symbol| {
                        // Resolved, not configured-only: a symbol nobody
                        // configured is served on a bundle that may carry a
                        // calendar, and its settlements are as real as a
                        // configured shape's.
                        sweep
                            .rivers
                            .resolve_profile(symbol)
                            .ok()
                            .and_then(|profile| profile.calendar.clone())
                            .map(|calendar| {
                                (symbol, calendar.settlement_instants(last_swept_ns, to_ns))
                            })
                    })
                    .flat_map(|(symbol, instants)| {
                        instants
                            .into_iter()
                            .map(move |instant| (Arc::clone(symbol), instant))
                    })
                    .collect();
                let rivers = Arc::clone(&sweep.rivers);
                let reads =
                    run_blocking(move || read_marks(&symbols, &settlements, to_ns, &rivers));
                let reads = tokio::select! {
                    reads = reads => reads.flatten(),
                    _ = completion.changed() => break 'passes,
                };
                let frontier = frontier_after(last_swept_ns, to_ns, reads.is_some());
                boat.last_swept_ns
                    .store(frontier, std::sync::atomic::Ordering::Release);
                // The high and low this river actually reached since the last
                // pass, taken from the tape thread. Taken AFTER the mark read,
                // so the span it closes is the one the mark closes too, and
                // taken once per pass whatever the passenger count - it is a
                // property of the water, like the walk.
                let span = boat.extremes.take();
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
                // One pass PER PASSENGER over the shared reading. Each ledger
                // books its own fills, settles its own positions and marks its
                // own exposure against the same prices - which is what makes the
                // tape common and the money private. Deliveries are separate
                // too, since `deliver` attributes by order ownership anyway.
                //
                // The settlement marks are cloned per passenger rather than
                // moved: a settlement instant belongs to the CALENDAR, so every
                // ledger holding that symbol crosses it.
                let symbol_key = mogwai_protocol::Symbol::from(symbol.as_str());
                let extremes: Vec<_> = span
                    .map(|span| {
                        vec![(
                            mogwai_protocol::Symbol::clone(&symbol_key),
                            span.high_px,
                            span.low_px,
                        )]
                    })
                    .unwrap_or_default();
                for (index, passenger) in seated.iter().enumerate() {
                    let mut engine = passenger.engine.lock().await;
                    let (events, emitted, originated) = apply_engine_pass_on_clock(
                        &mut engine,
                        &results[index],
                        settlement_marks.clone(),
                        &marks,
                        &extremes,
                        last_swept_ns,
                        to_ns,
                        session_closed.as_deref(),
                        boat.sim,
                    );
                    // The account's own rules, judged against the equity this
                    // pass just produced. HERE rather than after delivery,
                    // because a breach flattens - and a client must not be told
                    // its position is open in one batch and gone in the next
                    // when both describe the same instant.
                    let mut events = events;
                    let (emitted, originated, terminated) = enforce_policy(
                        passenger,
                        &mut engine,
                        &mut events,
                        &symbol_key,
                        span,
                        to_ns,
                        emitted,
                        originated,
                    );
                    let shape = engine.book_shape();
                    drop(engine);
                    if !events.is_empty() {
                        deliver(&sweep.run, &shape, &events, emitted, originated, to_ns);
                    }
                    // A TERMINATING breach on the venue's ONLY account ends the
                    // run: its one account is dead, so there is nothing left to
                    // serve, which is the same "no client, no job" rule that
                    // governs disconnection. Announced AFTER the batch is
                    // delivered, so the client learns why rather than seeing a
                    // bare close.
                    //
                    // Conditioned on there being only one account, deliberately.
                    // On a shared exchange one subagent breaching must not take
                    // down the batch, and the count is the only thing that
                    // distinguishes the two modes at runtime.
                    if terminated && seated.len() == 1 {
                        tracing::warn!(
                            account = %passenger.account_id.as_str(),
                            "the venue's only account terminated; ending the run",
                        );
                        sweep
                            .run
                            .complete(to_ns, to_ns.saturating_sub(sweep.run.started_ns));
                        break 'passes;
                    }
                }
            }
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

#[cfg(test)]
fn apply_engine_pass(
    engine: &mut mogwai_engine::Engine,
    results: &[ScanResult],
    settlement_marks: Vec<(mogwai_protocol::Symbol, u64, rust_decimal::Decimal)>,
    marks: &[(mogwai_protocol::Symbol, rust_decimal::Decimal)],
    to_ns: u64,
) -> (Vec<ServerMessage>, usize, usize) {
    apply_engine_pass_on_clock(
        engine,
        results,
        settlement_marks,
        marks,
        // No tape under these callers, so no span: the trail follows the mark,
        // which is the pre-extremes behaviour.
        &[],
        // No funding span and no session close: these callers are the unit
        // tests, which drive settlement and marking rather than the clock.
        to_ns,
        to_ns,
        None,
        mogwai_protocol::SimClock {
            sim_epoch_ns: 0,
            wall_anchor_ns: 0,
            speed: 1.0,
        },
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "one pass over one ledger needs every one of these: the scans, the settlement and mark prices, the swept span for funding, the session close for expiry, and the clock to stamp with"
)]
fn apply_engine_pass_on_clock(
    engine: &mut mogwai_engine::Engine,
    results: &[ScanResult],
    settlement_marks: Vec<(mogwai_protocol::Symbol, u64, rust_decimal::Decimal)>,
    marks: &[(mogwai_protocol::Symbol, rust_decimal::Decimal)],
    extremes: &[(
        mogwai_protocol::Symbol,
        rust_decimal::Decimal,
        rust_decimal::Decimal,
    )],
    from_ns: u64,
    to_ns: u64,
    session_closed: Option<&str>,
    sim: mogwai_protocol::SimClock,
) -> (Vec<ServerMessage>, usize, usize) {
    let (mut events, emitted) = engine.apply_scans_on_clock(results, to_ns, sim);
    let mut originated = 0;
    for (symbol, instant, px) in settlement_marks {
        let settled = engine.settle(&[(symbol, px)], instant);
        originated += settled.originated_orders;
        events.extend(settled.events);
    }
    let marked = engine.mark_over(marks, extremes, to_ns);
    // Time-driven expiry, which has nothing to do with triggers: a Gtd limit
    // nothing ever approached must still stop resting at its instant, and a Day
    // order must stop resting when its session closes whether or not the tape
    // came near it.
    events.extend(engine.expire_orders(to_ns, session_closed, to_ns));
    // Marked FIRST, then funded: funding is paid on notional at the mark, so
    // paying before the mark moved would charge this interval at the last
    // interval's price.
    let funded = engine.apply_funding(from_ns, to_ns, to_ns);
    events.extend(funded.events);
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

/// Hand one executed batch to the connections it belongs to.
///
/// Execution is run-scoped; DELIVERY stays per connection, because `ExecLanes`
/// is per connection. A connection whose reservation is refused gets the
/// ordinary `AdmissionRejected` on its priority lane and learns the real state
/// from `QueryOrders`/`QueryFills`; the EXECUTION is never rolled back. A
/// client's byte budget does not get to decide whether the market traded
/// through a price, and making it decide is what would wedge a book permanently
/// once a batch outgrew the fixed per-connection budget.
///
/// ATTRIBUTED, not broadcast. An order-scoped frame reaches only the connection
/// that submitted the order; everything else - the account snapshot, anything
/// the venue says about itself - still reaches all of them. This used to
/// broadcast unconditionally, so a socket received `OrderFilled` for orders
/// another socket placed. That was invisible while one connection per venue was
/// the only shape, and it is the first of the three channels through which
/// passengers can currently observe one another.
///
/// The reservation is taken against the UNFILTERED batch size, so a connection
/// reserves for frames it may not receive. Over-reserving is the safe direction:
/// sizing per connection would mean walking the ownership table once per lane
/// before knowing what to reserve, and a refusal costs the client only a
/// requery.
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
    // Resolved once for the batch rather than once per lane: the lookup takes
    // the run's ownership mutex, and a batch crossing N connections would
    // otherwise take it N times for the same answers.
    let owners: Vec<Option<String>> = events
        .iter()
        .map(|event| crate::run::addressed_order(event).and_then(|id| run.order_owner(id)))
        .collect();
    for bound in run.bound_lanes() {
        let mine: Vec<ServerMessage> = events
            .iter()
            .zip(&owners)
            .filter(|(_, owner)| {
                owner
                    .as_deref()
                    .is_none_or(|owner| owner == bound.account_id)
            })
            .map(|(event, _)| event.clone())
            .collect();
        if mine.is_empty() {
            continue;
        }
        let Some(reservation) = bound.lanes.reserve_swept(shape, emitted, originated) else {
            if refuse(&bound.lanes, subject.clone(), ts).is_err() {
                closed.push(bound.id);
            }
            continue;
        };
        if bound
            .lanes
            .submit_produced(reservation, Instant::now(), None, mine)
            .is_err()
        {
            closed.push(bound.id);
        }
    }
    // No ownership bookkeeping here, deliberately. A sweep-produced order is one
    // the VENUE originated (a liquidation), so there is nobody to claim it for,
    // and a claim is retired when its connection is released rather than when
    // its order ends - a terminal order still has to be attributable, because
    // `QueryOrders` reports terminal rows by design.
    // A lane whose receiver is gone is a connection that is already tearing
    // down; retiring it here means a wedged socket cannot make every later pass
    // pay for it.
    for id in closed {
        run.release_lanes(id);
    }
}

/// Judge one account against its own policy, and act if a rule fired.
///
/// Returns the possibly-grown admission counts, because a flatten produces
/// venue-originated orders and fills that the delivery reservation has to cover.
///
/// A BREACH FLATTENS AND THEN LOCKS. Flattening is the enforcement: the whole
/// point is that a strategy which would have been liquidated actually is. The
/// lock is what the breach action decides - until the next reset for a daily
/// limit, forever for a trailing drawdown - and it is read by the order-entry
/// gate rather than here.
///
/// EVALUATED AT TICK RESOLUTION, which is what `span` buys. Equity is linear in
/// the price of the one instrument an account can be holding, so its extreme
/// over the span is attained at a price extreme: replaying the span's two
/// extremes IN THE ORDER THE TAPE REACHED THEM reproduces what a per-tick walk
/// would have found, at two valuations rather than thousands. A spike that
/// opened and closed between two passes now spends drawdown budget, and a
/// collapse that recovered before the pass now breaches, both of which they did
/// at the venue being modelled.
///
/// The CLOSING equity is observed last regardless, because that is the reading
/// the published risk state has to agree with.
#[expect(
    clippy::too_many_arguments,
    reason = "one account's judgement needs the ledger, the batch it rides on, the span the tape covered and the admission counts it may grow"
)]
fn enforce_policy(
    passenger: &crate::run::Passenger,
    engine: &mut mogwai_engine::Engine,
    events: &mut Vec<ServerMessage>,
    symbol: &mogwai_protocol::Symbol,
    span: Option<crate::extremes::PriceSpan>,
    to_ns: u64,
    emitted: usize,
    originated: usize,
) -> (usize, usize, bool) {
    let mut ledger = passenger
        .risk
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(currency) = ledger.currency().map(str::to_owned) else {
        // Unpoliced. No equity is computed at all, so an unpoliced account pays
        // nothing for a feature it does not use.
        return (emitted, originated, false);
    };
    let Some(equity) = crate::risk::equity_in(engine, &currency) else {
        // The account holds value this policy cannot express. Order entry
        // refuses what would create that state, so reaching here means an
        // account acquired it another way - a venue-originated liquidation
        // partial, say. Enforcing against a wrong number would be worse than
        // not enforcing, so this warns loudly and declines rather than guessing.
        tracing::warn!(
            account = %passenger.account_id.as_str(),
            %currency,
            "cannot value this account in its policy currency; risk is not enforced this pass",
        );
        return (emitted, originated, false);
    };
    // The span's extremes first, in time order, then the close. A breach found
    // at an extreme is the FIRST verdict returned: the account was liquidated at
    // that instant, so a later reading cannot un-breach it, and `observe`
    // refuses to re-evaluate a breached ledger anyway.
    let mut verdict = crate::risk::Verdict::Clear;
    for (px, ts) in span.map(|span| span.in_time_order()).unwrap_or_default() {
        let Some(equity) =
            engine.valuation_at(&currency, &[(mogwai_protocol::Symbol::clone(symbol), px)])
        else {
            continue;
        };
        if let crate::risk::Verdict::Breached(breach) = ledger.observe(equity, ts) {
            verdict = crate::risk::Verdict::Breached(breach);
            break;
        }
    }
    if verdict == crate::risk::Verdict::Clear {
        verdict = ledger.observe(equity, to_ns);
    }
    drop(ledger);
    let crate::risk::Verdict::Breached(breach) = verdict else {
        return (emitted, originated, false);
    };
    tracing::warn!(
        account = %passenger.account_id.as_str(),
        rule = ?breach.rule,
        action = ?breach.action,
        equity = %breach.equity,
        threshold = %breach.threshold,
        "an account breached its risk policy",
    );
    let flattened = engine.liquidate_all(to_ns);
    let added = flattened.events.len();
    events.extend(flattened.events);
    (
        emitted + added,
        originated + flattened.originated_orders,
        breach.action == mogwai_protocol::risk::BreachAction::Terminate,
    )
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

    /// A fill on one connection's order must not reach another connection.
    ///
    /// The whole point of the attribution in `deliver`: before it, every bound
    /// lane received every sweep-produced frame, so a passenger was told about
    /// orders it never placed. Reverting the filter makes the second assertion
    /// fail - the unrelated connection receives the fill.
    #[tokio::test(flavor = "current_thread")]
    async fn a_swept_fill_reaches_only_the_connection_that_submitted_the_order() {
        let run = crate::run::test_run();
        let (mine, mut my_rx) = ExecLanes::detached();
        let (theirs, mut their_rx) = ExecLanes::detached();
        run.bind_lanes(mine.clone(), "MOGWAI-001");
        run.bind_lanes(theirs.clone(), "MOGWAI-002");
        let order: mogwai_protocol::VenueOrderId = "V-1".into();
        run.claim_order(order.clone(), "MOGWAI-001");

        // A PARTIAL fill, so the batch does not also retire the claim it is
        // being attributed by; the assertion is about delivery, not cleanup.
        let events = vec![ServerMessage::OrderFilled(mogwai_protocol::OrderFilled {
            client_order_id: "C-1".into(),
            venue_order_id: order,
            trade_id: "T-1".into(),
            symbol: mogwai_protocol::Symbol::from("BTCUSDT"),
            position_id: None,
            side: Side::Buy,
            last_qty: Decimal::ONE,
            last_px: Decimal::from(100),
            leaves_qty: Decimal::ONE,
            commission: Decimal::ZERO,
            commission_currency: "USDT".into(),
            liquidity_side: mogwai_protocol::LiquiditySide::Taker,
            ts_event: 1,
        })];
        deliver(
            &run,
            &mogwai_protocol::sizing::BookShape {
                balances: 1,
                positions: 1,
                margins: 1,
                open_orders: 1,
                closed_orders: 1,
                recorded_fills: 1,
            },
            &events,
            1,
            0,
            1,
        );

        assert!(
            my_rx.held_rx.try_recv().is_ok(),
            "the submitting connection must receive its own fill"
        );
        assert!(
            their_rx.held_rx.try_recv().is_err(),
            "a connection that submitted nothing must not learn about another's fill"
        );
    }

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
                basis: Default::default(),
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
            trail_offset: None,
            reduce_only: false,
            post_only: false,
            time_in_force: TimeInForce::Gtc,
            expire_time: None,
            link: None,
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

    /// The account for the tick-resolution tests: long one MNQ at 21,000 under
    /// a trailing drawdown, on a venue whose engine already holds the position.
    async fn policed_passenger(
        run: &Arc<crate::run::Run>,
        amount: u64,
    ) -> Arc<crate::run::Passenger> {
        let account = AccountId::parse("RISK-001").unwrap();
        run.open_account(
            &account,
            HashMap::from([("USD".to_string(), Decimal::from(10_000))]),
            mogwai_protocol::risk::AccountPolicy {
                trailing_drawdown: Some(mogwai_protocol::risk::TrailingDrawdown {
                    amount: Decimal::from(amount),
                    basis: mogwai_protocol::risk::TrailingBasis::PeakEquity,
                    lock_at_equity: None,
                    on_breach: mogwai_protocol::risk::BreachAction::Terminate,
                }),
                daily_loss_limit: None,
                reset_minute_utc: 0,
                currency: Some("USD".to_owned()),
            },
        )
        .expect("a fresh account opens");
        let passenger = run.passenger(&account);
        *passenger.engine.lock().await = engine_with_position();
        passenger
    }

    /// THE GAP THIS CLOSES. A spike that opened and closed entirely between two
    /// sweep passes used to be invisible: the pass saw only the closing mark, so
    /// the account kept drawdown room it had actually spent. The span carries
    /// the high, so the ratchet sees it.
    ///
    /// One MNQ long at 21,000 with a 2 multiplier: a 100-point spike is 200
    /// dollars of equity, so a 500-dollar trail that ratcheted spends 200 of it.
    #[tokio::test(flavor = "current_thread")]
    async fn a_spike_between_two_passes_spends_drawdown_budget() {
        let run = crate::run::test_run();
        let passenger = policed_passenger(&run, 500).await;
        let mut engine = passenger.engine.lock().await;
        let mut events = Vec::new();
        let symbol = mogwai_protocol::Symbol::from("MNQ");
        // The tape spiked to 21,100 and came back to 21,000, which is the only
        // price a mark-cadence evaluation would ever have seen.
        engine.mark(
            &[(
                mogwai_protocol::Symbol::clone(&symbol),
                Decimal::from(21_000),
            )],
            10,
        );
        let span = crate::extremes::PriceSpan {
            high_px: Decimal::from(21_100),
            high_ns: 5,
            low_px: Decimal::from(21_000),
            low_ns: 9,
        };
        let (_, _, terminated) = enforce_policy(
            &passenger,
            &mut engine,
            &mut events,
            &symbol,
            Some(span),
            10,
            0,
            0,
        );
        assert!(!terminated, "the spike breaches nothing on its own");
        let state = passenger
            .risk
            .lock()
            .unwrap()
            .state(engine.valuation_in("USD").expect("valuable"));
        assert_eq!(
            state.peak_equity,
            Decimal::from(10_200),
            "the peak ratcheted to the spike, not to the close"
        );
        assert_eq!(
            state.trailing_remaining,
            Some(Decimal::from(300)),
            "flat on the pass and 200 of the 500 budget is gone",
        );
    }

    /// The other half: a COLLAPSE that recovered before the pass. A
    /// mark-cadence evaluation sees an account comfortably inside its floor; the
    /// account was liquidated at the venue being modelled.
    #[tokio::test(flavor = "current_thread")]
    async fn a_collapse_that_recovered_before_the_pass_still_breaches() {
        let run = crate::run::test_run();
        let passenger = policed_passenger(&run, 500).await;
        let mut engine = passenger.engine.lock().await;
        let mut events = Vec::new();
        let symbol = mogwai_protocol::Symbol::from("MNQ");
        engine.mark(
            &[(
                mogwai_protocol::Symbol::clone(&symbol),
                Decimal::from(21_000),
            )],
            10,
        );
        // Down 400 points - 800 dollars, well through a 500-dollar floor - and
        // back before the pass looked.
        let span = crate::extremes::PriceSpan {
            high_px: Decimal::from(21_000),
            high_ns: 9,
            low_px: Decimal::from(20_600),
            low_ns: 5,
        };
        let (_, _, terminated) = enforce_policy(
            &passenger,
            &mut engine,
            &mut events,
            &symbol,
            Some(span),
            10,
            0,
            0,
        );
        assert!(
            terminated,
            "the account crossed its floor inside the span and is dead",
        );
        assert!(
            passenger.risk.lock().unwrap().is_locked(),
            "a terminating breach locks the account"
        );
    }

    /// A trailing stop follows the SPAN'S HIGH rather than its closing mark,
    /// which is the same tick-resolution fix on the order side.
    #[test]
    fn a_trailing_stop_ratchets_to_the_span_high_not_the_close() {
        let mut engine = engine_with_position();
        let trail = SubmitOrder {
            client_order_id: "TRAIL".into(),
            symbol: "MNQ".into(),
            position_id: None,
            side: Side::Sell,
            order_type: OrderType::TrailingStopMarket,
            quantity: Decimal::ONE,
            price: None,
            trigger_price: Some(Decimal::from(20_900)),
            trail_offset: Some(Decimal::from(100)),
            reduce_only: true,
            post_only: false,
            time_in_force: TimeInForce::Gtc,
            expire_time: None,
            link: None,
        };
        engine.process(ClientMessage::SubmitOrder(trail), 2);
        let symbol = mogwai_protocol::Symbol::from("MNQ");
        // Spiked to 21,100 and closed back at 21,000.
        engine.mark_over(
            &[(
                mogwai_protocol::Symbol::clone(&symbol),
                Decimal::from(21_000),
            )],
            &[(symbol, Decimal::from(21_100), Decimal::from(21_000))],
            10,
        );
        let trigger = engine
            .open_orders()
            .iter()
            .find(|order| order.submit.client_order_id == "TRAIL")
            .and_then(|order| order.submit.trigger_price)
            .expect("the trail still rests");
        assert_eq!(
            trigger,
            Decimal::from(21_000),
            "the trail followed the spike's high less its offset, not the close",
        );
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
            trail_offset: None,
            reduce_only: true,
            post_only: false,
            time_in_force: TimeInForce::Gtc,
            expire_time: None,
            link: None,
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
