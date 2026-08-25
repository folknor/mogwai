// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The venue re-checking the run's resting limits against the tape.
//!
//! Spawned once at boot, unconditionally, because every resting limit now
//! carries a trigger only a tape walk can advance. A pass with nothing resting
//! is still just one lock acquisition and a `continue`.
//!
//! Owned by the run rather than by an account or a passenger: the run holds
//! every ledger, and a passenger-owned sweep would freeze a disconnected consumer's
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
use mogwai_protocol::{AdmissionSubject, VenueMessage};

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
/// runs off the engine lock and on `spawn_blocking` or it stalls both order
/// entry and a runtime worker. The engine re-validates every result against its
/// order revision in phase three, which is what makes the off-lock gap safe.
pub(crate) fn spawn_fill_sweeper(sweep: FillSweep) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut completion = sweep.run.completion();
        // Earliest-deadline schedule, one task. The sweep interval is simulated
        // milliseconds, so it converts through a clock before it becomes a wall
        // sleep - and since `/ws?speed=` landed, the boats on one venue no
        // longer share a speed, so there is no single conversion. Each boat
        // therefore carries its own next-due instant and the task sleeps to the
        // earliest of them. Deliberately not one task per boat: N tasks contend
        // the single engine lock and multiply the completion fan-out, to buy
        // cadence granularity that settlement correctness does not rest on
        // (per-boat `to_ns` and per-boat `last_swept_ns` already carry that).
        //
        // Keyed by `BoatKey`, not by the `Arc`'s address: an address is reused
        // after the last ticket drops a boat, so a newly placed boat could
        // inherit a departed one's due instant.
        let mut next_due: HashMap<crate::boatyard::BoatKey, (Arc<crate::boatyard::Boat>, Instant)> =
            HashMap::new();
        // Labelled because the per-boat body below breaks on completion: an
        // unlabelled break there would leave only the boat loop and the
        // sweeper would keep walking a completed run forever.
        'passes: loop {
            // Re-derived every pass: boats appear and leave under this task. A
            // boat placed mid-pass is due immediately on the next one, which is
            // the latency it had under the old shared cadence too.
            let now = Instant::now();
            let boats = sweep.run.boatyard.boats();
            let placed_boats: HashSet<_> = boats.iter().map(|boat| boat.key()).collect();
            next_due.retain(|key, _| placed_boats.contains(key));
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
            // One acquisition per account, so each ledger's scans and mark
            // symbols come from one consistent read of it.
            //
            // Gathered across every account before the walk, deliberately: the
            // tape walk is the expensive part of a pass and is a property of the
            // river, not of any ledger, so N accounts resting orders on one
            // symbol still walk it once. Results are grouped back per account
            // afterwards.
            // Attached accounts only. An account nobody is reading is frozen:
            // its orders do not rest, its positions do not mark, its funding
            // does not accrue and its policy cannot liquidate somebody who is
            // not there. That is a deliberate departure from a real venue, where
            // being away is no defence, and it is the right one here - mogwai
            // exists to exercise a consumer's live path, not to run an account
            // nobody is trading. The consequence to state in any claim is that a
            // run spanning a disconnect has a gap in its risk history.
            //
            // It also closes the boatless-river gap from the other side. Every
            // resting order belongs to the river its account is bound to, which
            // has a boat for as long as the account is attached, so a frozen
            // account is the only way an order can end up on a river nobody
            // reads - and a frozen account is skipped here rather than swept
            // against a clock that no longer exists.
            let all_accounts = sweep.run.accounts();
            // Whether this run holds exactly one ledger, which is what the
            // terminating breach below is conditioned on. Asked before the
            // frozen filter, and asked through a named function so the question
            // has one implementation a test can read rather than an expression
            // buried in a loop.
            let sole_ledger = holds_one_ledger(&all_accounts);
            let attached_accounts: Vec<_> = all_accounts
                .into_iter()
                .filter(|account_state| !account_state.is_frozen())
                .collect();
            // Every boat placed right now, re-read after the freeze filter above
            // rather than reused from `next_due`. An upgrade places its boat
            // before it attaches the connection, so an account observed attached
            // here has its boat in this list; `next_due` was sampled before the
            // pass slept and can be missing a boat boarded during that sleep,
            // which would make the cancellation below fire on an account that
            // had only just sat down.
            let placed = sweep.run.boatyard.boats();
            let mut scans: Vec<(usize, PendingScan)> = Vec::new();
            let mut mark_symbols = Vec::new();
            let venue_now = sim_now_ns(sweep.run.sim);
            for (index, account_state) in attached_accounts.iter().enumerate() {
                let mut engine = account_state.engine.lock().await;
                // What this account's own cursors are actually reading right
                // now. Its order outside this set rests on a river no cursor of
                // its own reads: the sweep decides a scan only for an account
                // seated on the due boat, so an order on anyone else's river is
                // never filled, never expired and never cancelled, and cannot be
                // told apart from an order the tape has not reached. The venue
                // refuses to leave it there rather than letting it sit forever -
                // the consumer is attached, so it can be told.
                //
                // Per account, not per venue, because the predicate that decides
                // cancellation must be the predicate that decides sweeping. A
                // venue-wide set is "rivers with a clock" while the sweep asks
                // "rivers with this account's clock", and those differ the moment
                // two accounts ride different symbols or one river at two
                // cadences. A frozen account is not in this loop at all, and its
                // book survives for the socket that returns to it.
                let readable = readable_symbols(
                    placed.iter().map(|boat| (boat.symbol(), boat.key())),
                    |key| account_state.is_seated_on(key),
                );
                // Stamped on the venue clock, which is the only one that
                // answers here: the order being cancelled is on a river with no
                // boat, so there is no river clock to date it by.
                let cancelled = engine.cancel_unreadable_orders(&readable, venue_now);
                if !cancelled.is_empty() {
                    let shape = engine.book_shape();
                    deliver_produced(
                        &sweep.run,
                        account_state.account_id.as_str(),
                        &shape,
                        &cancelled,
                        cancelled.len(),
                        0,
                        venue_now,
                    );
                }
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
            // re-armed on its clock and floored at `MIN_SWEEP_WALL` as before.
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
            // The sweep is symbol-keyed by construction, and a survey once
            // flagged this walk as the likeliest hidden single-symbol
            // assumption in the venue and found the opposite. Pending scans are
            // grouped by the symbol of the boat they belong to, the walk runs
            // once per boat and therefore once per symbol, and the marks and
            // settlements below look each symbol up in the river store rather
            // than assuming one. Do not re-derive that suspicion from the shape
            // of this loop.
            for boat in due_boats {
                let symbol = boat.symbol().to_owned();
                let to_ns = sim_now_ns(boat.sim);
                // Scans on this river from accounts actually riding this
                // boat. The walk is a property of the water, but the clock is
                // a property of the cursor: applying a fast boat's now to a
                // slow account (or the reverse) would fill one ledger twice
                // against two clocks.
                let boat_key = boat.key();
                let boat_scans: Vec<(usize, PendingScan)> = scans
                    .iter()
                    .filter(|(index, scan)| {
                        scan.symbol.as_ref() == symbol
                            && attached_accounts[*index].is_seated_on(&boat_key)
                    })
                    .cloned()
                    .collect();
                let mut results: Vec<Vec<ScanResult>> = vec![Vec::new(); attached_accounts.len()];
                let rivers = Arc::clone(&sweep.rivers);
                let scans_for_walk: Vec<PendingScan> =
                    boat_scans.iter().map(|(_, scan)| scan.clone()).collect();
                // The boat's own river, not a key re-derived from the label it
                // carries. A sweep pass belongs to one boat, that boat holds
                // the exact water its passengers are reading, and a scan
                // decided against any other river would fill an order on prints
                // its owner never saw.
                let walk_river = boat.key().river().clone();
                let walked = tokio::task::spawn_blocking(move || {
                    fills::scan_triggers(&walk_river, &scans_for_walk, to_ns, &rivers)
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
                // This boat's own symbol, plus its funding index if that
                // river already exists. Reading an index must never spend a
                // river nobody asked for: `last_trade_at_or_before`
                // materializes, so an unmaterialized index is left unread
                // and the rate stays at the configured interest.
                //
                // The materialized list is only consulted when this boat
                // actually names an index: it takes a mutex and allocates
                // every river, and every non-perp would otherwise pay that
                // on every pass.
                //
                // An index is the second selector question the river fork has to
                // answer, and it is worth naming beside the mark one below. A
                // perpetual's boat names an index symbol it does not itself
                // ride, so it holds no key for that index and cannot hold one.
                // While a label names one river the registry answers
                // unambiguously; once generator havoc enters river identity,
                // which fork of the index prices funding is a real choice with
                // no passenger to ask. It differs from history in that it is
                // internal configuration rather than necessarily a wire field.
                let index = sweep
                    .rivers
                    .resolve_profile(&symbol)
                    .ok()
                    .and_then(|profile| profile.def.class.funding())
                    .and_then(|terms| terms.index_symbol)
                    .filter(|index| {
                        sweep
                            .rivers
                            .materialized_symbols()
                            .iter()
                            .any(|existing| existing == index)
                    });
                let symbols: Vec<_> = mark_symbols
                    .iter()
                    .filter(|candidate| {
                        candidate.as_ref() == symbol
                            || index
                                .as_ref()
                                .is_some_and(|index| candidate.as_ref() == index)
                    })
                    .cloned()
                    .collect();
                // Whether this boat's river crossed a session close in the span
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
                // Settlements belong to this boat's instrument, never to a
                // funding index. `read_marks` fails the whole pass if any
                // settlement instant is unpriceable, and an index can carry a
                // calendar the account holds nothing in; walking that list
                // would stall the perp boat on a symbol it is not trading.
                let boat_symbol = mogwai_protocol::Symbol::from(symbol.as_str());
                let settlements: Vec<_> = std::iter::once(&boat_symbol)
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
                let unpaced = boat_key.speed_micros() == 0;
                let span_river = boat_key.river().clone();
                let reads = run_blocking(move || {
                    let marks = read_marks(&symbols, &settlements, to_ns, &rivers)?;
                    let span = if unpaced {
                        fills::price_span(&span_river, last_swept_ns, to_ns, &rivers)?
                    } else {
                        None
                    };
                    Some((marks, span))
                });
                let reads = tokio::select! {
                    reads = reads => reads.flatten(),
                    _ = completion.changed() => break 'passes,
                };
                let frontier = frontier_after(last_swept_ns, to_ns, reads.is_some());
                boat.last_swept_ns
                    .store(frontier, std::sync::atomic::Ordering::Release);
                let Some(((marks, settlement_marks), span)) =
                    commit_pass(reads, &boat.extremes, unpaced)
                else {
                    // The reading task died, or one of this span's settlement
                    // instants had no readable price. Abandoning the whole pass is
                    // the only safe response either way: the scan results are
                    // re-derivable (the engine still holds every pending scan at
                    // its unadvanced `from_ns`), while the settlement instants this
                    // interval crossed exist nowhere but in the span
                    // `last_swept_ns..to_ns`.
                    continue;
                };
                // One pass per account over the shared reading. Each ledger
                // books its own fills, settles its own positions and marks its
                // own exposure against the same prices - which is what makes the
                // tape common and the money private. Deliveries are separate
                // too, since `deliver` attributes each frame to the account it
                // is about - by the frame's own account id where it has one, and
                // by order ownership otherwise.
                //
                // The settlement marks are cloned per account rather than
                // moved: a settlement instant belongs to the calendar, so every
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
                for (index, account_state) in attached_accounts.iter().enumerate() {
                    if !account_state.is_seated_on(&boat_key) {
                        continue;
                    }
                    let mut engine = account_state.engine.lock().await;
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
                    // pass just produced. Here rather than after delivery,
                    // because a breach flattens - and a consumer must not be told
                    // its position is open in one batch and gone in the next
                    // when both describe the same instant.
                    let mut events = events;
                    let (emitted, originated, terminated) = enforce_policy(
                        account_state,
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
                        deliver_produced(
                            &sweep.run,
                            account_state.account_id.as_str(),
                            &shape,
                            &events,
                            emitted,
                            originated,
                            to_ns,
                        );
                    }
                    // A terminating breach on the venue's only account ends the
                    // run: its one account is dead, so there is nothing left to
                    // serve, which is the same "no consumer, no job" rule that
                    // governs disconnection. Announced after the batch is
                    // delivered, so the consumer learns why rather than seeing a
                    // bare close.
                    //
                    // Conditioned on there being only one account, deliberately.
                    // On a shared exchange one subagent breaching must not take
                    // down the batch, and the count is the only thing that
                    // distinguishes the two modes at runtime.
                    //
                    // Accounts held, not accounts attached, and the difference
                    // is the whole gate. A frozen account has no boat and is
                    // filtered out of the sweep above, so counting the attached
                    // set let one breaching account end a run whose other
                    // ledgers were merely between sockets - the shared-exchange
                    // case this is written to protect, failing exactly when the
                    // other passengers happened to be away. The venue holds
                    // those ledgers and a passenger can return to any of them,
                    // so they are the run, whether or not anyone is reading
                    // them this instant.
                    if terminated && sole_ledger {
                        tracing::warn!(
                            account = %account_state.account_id.as_str(),
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
/// Both are exact-instant last-print reads rather than `MarketReadingCache`
/// lookups. That cache buckets by fill-sweep interval, which is a defensible
/// coarseness for a volatility band and is not one for a mark price: unrealized
/// P and L and the margin evaluation that follows it would freeze for every pass
/// sharing a bucket, which under an accelerated clock is several of them.
///
/// The two halves fail differently, and that asymmetry is the point. An
/// unreadable ordinary mark costs one pass of unrealized P and L freshness and
/// is asked again five milliseconds later, so it is dropped. An unreadable
/// settlement price cannot be asked again: `last_swept_ns` is about to move past
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
    // A mark names an instrument, not a river, and this is where that becomes a
    // problem the fork has to answer. These symbols come from the ledger - a
    // position is aggregated per instrument - so nothing here carries the river
    // the position was traded against. Today one label resolves to one river and
    // the two are the same answer; once generator havoc enters river identity,
    // one account can hold a position in a symbol while its passengers watch
    // different realizations of it, and a single mark cannot be right for both.
    // Resolved here, at one named boundary, rather than by letting the readers
    // take labels: that keeps the ambiguity visible in one place instead of
    // spread across every water read.
    let key_for = |symbol: &mogwai_protocol::Symbol| rivers.key_for_symbol(symbol).ok();
    let marks: Vec<_> = symbols
        .iter()
        .filter_map(|symbol| {
            let river = key_for(symbol)?;
            fills::read_last(&river, to_ns, rivers).map(|px| (std::sync::Arc::clone(symbol), px))
        })
        .collect();
    let settlement_marks: Option<Vec<_>> = settlements
        .iter()
        .map(|(symbol, instant)| {
            let river = key_for(symbol)?;
            fills::read_last(&river, *instant, rivers)
                .map(|px| (std::sync::Arc::clone(symbol), *instant, px))
        })
        .collect();
    Some((marks, settlement_marks?))
}

/// Where the settlement frontier stands after a pass.
///
/// `last_swept_ns` is a watermark, and the only record that a span is still
/// owed settlement: the next pass asks the calendar for the instants inside
/// `last_swept_ns..to_ns` and nothing ever looks further back than that. So it
/// may only move over a span whose settlement prices were actually read.
/// Advancing it on a failed reading pass - which is what treating the failure
/// as an empty result does - retires every settlement instant the span crossed
/// without anyone having priced them, permanently and silently.
///
/// `read` is therefore the success of both halves: the reading task surviving,
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

/// Commit one pass to the reading it just took, and only then consume the
/// river's price span.
///
/// The frontier rule applied to a destructive read. `PriceExtremes::take` bumps
/// the epoch and clears the published span, so it retires the high and the low
/// the tape reached exactly the way `last_swept_ns` retires a settlement
/// interval - and it may therefore only run over a pass that is going to use
/// them. Taking it before the reading was checked threw the extremes away on a
/// failed pass, over an interval the unadvanced watermark then makes the next
/// pass re-sweep: the account was marked across that span with its extremes
/// silently dropped and the next span starting from its own first print, which
/// is precisely the spike-between-two-passes hole `extremes.rs` exists to
/// close, reopened on the failure path.
///
/// Ordering alone is what enforces this, so the two steps are here, in one
/// expression a reordering has to delete rather than move.
fn commit_pass(
    reads: Option<(MarkReads, Option<crate::extremes::PriceSpan>)>,
    extremes: &crate::extremes::PriceExtremes,
    unpaced: bool,
) -> Option<(MarkReads, Option<crate::extremes::PriceSpan>)> {
    let (reads, bounded_span) = reads?;
    let span = if unpaced {
        bounded_span
    } else {
        extremes.take()
    };
    Some((reads, span))
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
) -> (Vec<VenueMessage>, usize, usize) {
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
) -> (Vec<VenueMessage>, usize, usize) {
    let (mut events, emitted) = engine.apply_scans_on_clock(results, to_ns, sim);
    let mut originated = 0;
    for (symbol, instant, px) in settlement_marks {
        let settled = engine.settle(&[(symbol, px)], instant);
        originated += settled.originated_orders;
        events.extend(settled.events);
    }
    // Sale proceeds whose settlement period has run. Before the mark, so the
    // pass's single snapshot reports the freed cash rather than reporting it one
    // pass late.
    let settled_cash = engine.release_settled_cash(to_ns);
    let marked = engine.mark_over(marks, extremes, to_ns);
    // Time-driven expiry, which has nothing to do with triggers: a Gtd limit
    // nothing ever approached must still stop resting at its instant, and a Day
    // order must stop resting when its session closes whether or not the tape
    // came near it.
    events.extend(engine.expire_orders(to_ns, session_closed, to_ns));
    // Marked first, then funded: funding is paid on notional at the mark, so
    // paying before the mark moved would charge this interval at the last
    // interval's price.
    let funded = engine.apply_funding(from_ns, to_ns, to_ns);
    events.extend(funded.events);
    originated += marked.originated_orders;
    events.extend(marked.events);
    // Exactly one `AccountState` from this phase, recomputed after every
    // mutation in the phase. Choosing an existing snapshot by vector position
    // is insufficient: marking is computed before expiry and funding but its
    // events are appended after both, so the last appended snapshot can be the
    // oldest ledger state. The presence of a surviving snapshot still decides
    // whether one is emitted, preserving `DropNextAccountUpdate`.
    let last_state = events
        .iter()
        .rposition(|event| matches!(event, VenueMessage::AccountState(_)));
    if let Some(last_state) = last_state {
        let mut index = 0;
        events.retain(|event| {
            let keep = !matches!(event, VenueMessage::AccountState(_)) || index == last_state;
            index += 1;
            keep
        });
        let snapshot = engine.account_snapshot(to_ns);
        if let Some(state) = events
            .iter_mut()
            .find(|event| matches!(event, VenueMessage::AccountState(_)))
        {
            *state = VenueMessage::AccountState(snapshot);
        }
    } else if settled_cash {
        // Cash settling moves `free` and `locked` without moving `total` and
        // without producing an order event, so it is the one transition that
        // owes a snapshot nothing else in the pass would have taken. A consumer
        // watching its buying power is watching exactly this.
        events.push(VenueMessage::AccountState(engine.account_snapshot(to_ns)));
    }
    (events, emitted, originated)
}

/// Hand one executed batch to the connections it belongs to.
///
/// Execution is run-scoped; delivery stays per connection, because `ExecLanes`
/// is per connection. A connection whose reservation is refused gets the
/// ordinary `AdmissionRejected` on its priority lane and learns the real state
/// from `QueryOrders`/`QueryFills`; the execution is never rolled back. A
/// consumer's byte budget does not get to decide whether the market traded
/// through a price, and making it decide is what would wedge a book permanently
/// once a batch outgrew the fixed per-connection budget.
///
/// Attributed, not broadcast. An order-scoped frame reaches only the connection
/// that submitted the order, and an account-scoped one only the connections
/// bound to that account; what reaches everyone is what is genuinely about the
/// venue - a fault, a completion, a feed gap. This used to
/// broadcast unconditionally, so a socket received `OrderFilled` for orders
/// another socket placed. That was invisible while one connection per venue was
/// the only shape.
///
/// The account snapshot was the residual hole, and it was the expensive one.
/// Order attribution alone left `AccountState` unaddressed, so it fanned to
/// every lane - and the sweep takes one engine pass per account, so an
/// N-account venue sent each socket N snapshots per pass, N-1 of them somebody
/// else's balances and positions. Attribution is now [`crate::run::audience`],
/// an exhaustive classification with no catch-all, so the next ledger-owned
/// frame variant is a compile error rather than a silent broadcast.
///
/// The reservation is taken against the unfiltered batch size, so a connection
/// reserves for frames it may not receive. Over-reserving is the safe direction:
/// sizing per connection would mean walking the ownership table once per lane
/// before knowing what to reserve, and a refusal costs the consumer only a
/// requery.
/// Claim and then deliver a batch one account's ledger just produced.
///
/// The claim is fused to the delivery so no production site can take one
/// without the other: a venue-originated order in the batch (a liquidation
/// the venue minted) has no submitting connection, and the only instant its
/// account is knowable without ambient delivery context is here, where the
/// batch is still attached to the account whose engine pass produced it.
/// See [`Run::claim_produced_orders`] for the whole argument.
fn deliver_produced(
    run: &Arc<Run>,
    producer: &str,
    shape: &mogwai_protocol::sizing::BookShape,
    events: &[VenueMessage],
    emitted: usize,
    originated: usize,
    ts: u64,
) {
    run.claim_produced_orders(events, producer);
    deliver(run, shape, events, emitted, originated, ts);
}

fn deliver(
    run: &Arc<Run>,
    shape: &mogwai_protocol::sizing::BookShape,
    events: &[VenueMessage],
    emitted: usize,
    originated: usize,
    ts: u64,
) {
    let subject = (!events.is_empty()).then_some(AdmissionSubject::Frame);
    let mut closed = Vec::new();
    enum Route {
        Everyone,
        Account(String),
        Drop,
    }
    // Resolved once for the batch rather than once per lane: the ownership
    // lookup takes the run's mutex, and a batch crossing N connections would
    // otherwise take it N times for the same answers.
    let routes: Vec<Route> = events
        .iter()
        .map(|event| match crate::run::audience(event) {
            crate::run::Audience::Venue | crate::run::Audience::Unattributable => Route::Everyone,
            crate::run::Audience::Account(account) => Route::Account(account.as_str().to_owned()),
            // Every order-scoped frame reaching this point is claimed: the
            // dispatcher claims consumer submissions at acceptance, and
            // `deliver_produced` claims venue-originated orders for the
            // ledger that produced them. A miss is therefore a bug in whoever
            // built the batch, not a class of order - reported, then routed
            // to everyone, because an account missing its own fill is still
            // the worse wrong while the bug lives.
            crate::run::Audience::Order(id) => run.order_owner(id).map_or_else(
                || {
                    tracing::warn!(
                        venue_order_id = %id,
                        "an unclaimed order reached swept delivery; its frames were broadcast - \
                         the production site failed to claim it"
                    );
                    Route::Everyone
                },
                Route::Account,
            ),
            // A requester-scoped frame has no business in a swept batch: it
            // belongs to the connection that issued the request, which this
            // path cannot know, and broadcasting it would leak one consumer's
            // orders, fills or refusals to every other. Dropped loudly - the
            // defect is in whatever put it here, not in delivery.
            crate::run::Audience::Requester => {
                tracing::warn!(
                    ?event,
                    "a requester-scoped frame reached swept delivery and was dropped; \
                     it must be delivered on the issuing lane instead"
                );
                Route::Drop
            }
        })
        .collect();
    for bound in run.bound_lanes() {
        let mine: Vec<VenueMessage> = events
            .iter()
            .zip(&routes)
            .filter(|(_, route)| match route {
                Route::Everyone => true,
                Route::Account(owner) => *owner == bound.account_id,
                Route::Drop => false,
            })
            .map(|(event, _)| event.clone())
            .collect();
        if mine.is_empty() {
            continue;
        }
        let Some(reservation) = bound.lanes.reserve_swept(shape, emitted, originated) else {
            if refuse(&bound.lanes, subject.clone(), ts).is_err() {
                closed.push((bound.account_id.clone(), bound.id));
            }
            continue;
        };
        if bound
            .lanes
            .submit_produced(reservation, Instant::now(), None, mine)
            .is_err()
        {
            closed.push((bound.account_id.clone(), bound.id));
        }
    }
    // No ownership bookkeeping here, deliberately - but the reason changed in
    // 2026-08-20 and the old one is recorded so it is not re-derived: this
    // function used to argue a sweep-produced order had "nobody to claim it
    // for". It does - the account whose ledger produced it - and
    // `deliver_produced` claims it before this function runs, because the only
    // place the producer is knowable without ambient delivery context is at
    // production. Delivery itself stays a pure function of the batch. Claims
    // retire with the account, never on a terminal frame, because
    // `QueryOrders` reports terminal rows by design.
    // A lane whose receiver is gone is a connection that is already tearing
    // down; retiring it here means a wedged socket cannot make every later pass
    // pay for it.
    for (account_id, id) in closed {
        run.release_lanes(&account_id, id);
    }
}

/// Judge one account against its own policy, and act if a rule fired.
///
/// Returns the possibly-grown admission counts, because a flatten produces
/// venue-originated orders and fills that the delivery reservation has to cover.
///
/// A breach flattens and then locks. Flattening is the enforcement: the whole
/// point is that a strategy which would have been liquidated actually is. The
/// lock is what the breach action decides - until the next reset for a daily
/// limit, forever for a trailing drawdown - and it is read by the order-entry
/// gate rather than here.
///
/// Evaluated at tick resolution in `symbol`, which is what `span` buys. Equity
/// is linear in each price it depends on, so holding every other price fixed,
/// its extreme over the span is attained at one of `symbol`'s two price
/// extremes: replaying them in the order the tape reached them reproduces what
/// a per-tick walk of that river would have found, at two valuations rather
/// than thousands. A spike that opened and closed between two passes now spends
/// drawdown budget, and a collapse that recovered before the pass now breaches,
/// both of which they did at the venue being modelled.
///
/// The bound, since an account rides as many rivers as its passengers have
/// boarded. `span` carries the due boat's river alone; `valuation_at` marks
/// that one symbol at the extreme and values every other symbol the account
/// holds at its last read. So the cross-river component of equity is evaluated
/// at mark cadence, and the ratchet sees a partial reconstruction of the
/// interval ordered by whichever boat came due first. That is a stated bound of
/// the model, not a defect here. See `extremes` for the same statement at the
/// type that records the span.
///
/// The closing equity is observed last regardless, because that is the reading
/// the published risk state has to agree with.
#[expect(
    clippy::too_many_arguments,
    reason = "one account's judgement needs the ledger, the batch it rides on, the span the tape covered and the admission counts it may grow"
)]
fn enforce_policy(
    account_state: &crate::run::Account,
    engine: &mut mogwai_engine::Engine,
    events: &mut Vec<VenueMessage>,
    symbol: &mogwai_protocol::Symbol,
    span: Option<crate::extremes::PriceSpan>,
    to_ns: u64,
    emitted: usize,
    originated: usize,
) -> (usize, usize, bool) {
    let mut ledger = account_state
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
            account = %account_state.account_id.as_str(),
            %currency,
            "cannot value this account in its policy currency; risk is not enforced this pass",
        );
        return (emitted, originated, false);
    };
    // Resolve every valuation before mutating the risk ledger. A span is one
    // unit of policy evidence: consuming one extreme and silently skipping the
    // other would publish a judgement over a path the venue could not value.
    let mut readings = Vec::new();
    for (px, ts) in span.map(|span| span.in_time_order()).unwrap_or_default() {
        let Some(extreme_equity) =
            engine.valuation_at(&currency, &[(mogwai_protocol::Symbol::clone(symbol), px)])
        else {
            tracing::warn!(
                account = %account_state.account_id.as_str(),
                %currency,
                symbol = %symbol,
                "cannot value a price extreme in this account's policy currency; risk is not enforced this pass",
            );
            return (emitted, originated, false);
        };
        readings.push((extreme_equity, ts));
    }

    // The span's extremes first, in time order, then the close. The first
    // breach remains the action, but the close is still observed so a lock's
    // peak ratchet agrees with the last ledger reading.
    let mut verdict = crate::risk::Verdict::Clear;
    for (extreme_equity, ts) in readings {
        if let crate::risk::Verdict::Breached(breach) = ledger.observe(extreme_equity, ts) {
            verdict = crate::risk::Verdict::Breached(breach);
            break;
        }
    }
    let closing_verdict = ledger.observe(equity, to_ns);
    if verdict == crate::risk::Verdict::Clear {
        verdict = closing_verdict;
    }
    drop(ledger);
    let crate::risk::Verdict::Breached(breach) = verdict else {
        return (emitted, originated, false);
    };
    tracing::warn!(
        account = %account_state.account_id.as_str(),
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
        VenueMessage::AdmissionRejected {
            subject,
            reason: "execution output admission budget exhausted".into(),
            retryable: true,
            ts_event: ts,
        },
    )
}

/// Whether this run holds exactly one ledger.
///
/// A frozen account counts, and that is the whole of what this function is for.
/// The sweep skips frozen accounts because they have no boat and therefore no
/// clock to be swept against, so every other question in that loop is asked of
/// the attached set - and asking this one of the attached set is what let a
/// breaching account end a run whose other ledgers were merely between sockets.
/// The venue still holds those ledgers, with their positions, orders and
/// balances, and a returning passenger can claim any of them; they are the run
/// whether or not anyone is reading them at this instant.
///
/// It is a function rather than an expression in the loop so the question has
/// one implementation, with a name, that a test can read. Written inline it was
/// indistinguishable from the half-dozen other things that loop legitimately
/// asks of the attached set.
fn holds_one_ledger(accounts: &[Arc<crate::run::Account>]) -> bool {
    accounts.len() == 1
}

fn readable_symbols<'a>(
    boats: impl Iterator<Item = (&'a str, crate::boatyard::BoatKey)>,
    mut is_seated_on: impl FnMut(&crate::boatyard::BoatKey) -> bool,
) -> Vec<mogwai_protocol::Symbol> {
    let mut readable: Vec<_> = boats
        .filter(|(_, key)| is_seated_on(key))
        .map(|(symbol, _)| mogwai_protocol::Symbol::from(symbol))
        .collect();
    readable.sort();
    readable.dedup();
    readable
}

#[cfg(test)]
mod tests {
    use super::*;
    use mogwai_engine::{EngineConfig, MarginBreachAction, MarginPolicy, MarketReading};
    use mogwai_protocol::{
        AccountId, Command, Hit, InstrumentClass, InstrumentDef, OrderType, Side, SubmitOrder,
        TimeInForce, WireAssetClass,
    };
    use rust_decimal::Decimal;

    #[test]
    fn readable_is_scoped_to_the_account_seated_on_the_boat() {
        let run = crate::run::test_run();
        let btc = run.rivers.test_key("BTCUSDT");
        let btc_boat = crate::boatyard::BoatKey::new(btc.clone(), 1.0).unwrap();
        let mnq_boat = crate::boatyard::BoatKey::new(btc, 2.0).unwrap();
        let readable = readable_symbols(
            [("BTCUSDT", btc_boat.clone()), ("MNQ", mnq_boat)].into_iter(),
            |key| key == &btc_boat,
        );
        assert_eq!(readable, vec![mogwai_protocol::Symbol::from("BTCUSDT")]);
    }

    /// A terminating breach ends the run only when the run holds one ledger,
    /// and a frozen account is one the run still holds.
    ///
    /// The gate reads `accounts_held`, taken before the frozen filter. Reading
    /// the attached set instead - which is what it did - made the
    /// shared-exchange protection fail exactly when the other passengers
    /// happened to be between sockets: their accounts are skipped by the sweep,
    /// so the breaching one looks like the only account and completes a run
    /// whose other ledgers are still holding positions, orders and balances a
    /// returning socket can claim.
    ///
    /// What is asserted is the discriminator rather than the sweep, because the
    /// two counts differing is the whole of the defect: if a frozen account
    /// could never make them differ, the fix would be vacuous and this test
    /// would pass against the old code.
    #[tokio::test(flavor = "current_thread")]
    async fn a_frozen_account_is_still_an_account_this_run_holds() {
        let run = crate::run::test_run();
        let mine_account = AccountId::parse("MOGWAI-001").unwrap();
        let theirs_account = AccountId::parse("MOGWAI-002").unwrap();
        // An account is unattended until a connection of it is reading, so both
        // are admitted and bound and only one is then released. A test that
        // bound neither would find both frozen and prove nothing about the
        // split the gate turns on.
        let (_mine_attach, mine_conn) = crate::run::admit_for_test(&run, &mine_account, None);
        let (_theirs_attach, theirs_conn) = crate::run::admit_for_test(&run, &theirs_account, None);
        let (mine, _my_rx) = ExecLanes::detached_as(mine_conn.connection_id);
        let (theirs, _their_rx) = ExecLanes::detached_as(theirs_conn.connection_id);
        let mine_id = run.bind_lanes(mine, "MOGWAI-001", None);
        run.bind_lanes(theirs, "MOGWAI-002", None);
        let first = run.account(&mine_account);
        let second = run.account(&theirs_account);
        let symbol = mogwai_protocol::Symbol::from(run.default_symbol.as_ref());
        drop(
            run.resume(&first, &symbol, 1, mine_conn.resumed_from_freeze)
                .await,
        );
        drop(
            run.resume(&second, &symbol, 1, theirs_conn.resumed_from_freeze)
                .await,
        );

        // The first passenger leaves. Its ledger survives - that is what makes
        // a reconnect a continuation - but it stops being attended.
        run.release_lanes("MOGWAI-001", mine_id);

        let held = run.accounts();
        // The fixture's own premise first. If the release did not actually
        // freeze anything, the two sets below are equal and the assertion that
        // matters would hold for a reason that has nothing to do with the gate.
        assert_eq!(
            held.iter().filter(|account| !account.is_frozen()).count(),
            1,
            "exactly one of the two accounts must be frozen, or this fixture \
             cannot tell the two counts apart"
        );

        // Read through the gate's own function rather than recomputed here. A
        // test that counted `accounts()` itself would pass whatever the gate
        // did, which is how the first version of this test came to pass against
        // the defect it names.
        assert!(
            !holds_one_ledger(&held),
            "a run holding two ledgers must not end on one account's breach, \
             even while the other is between sockets"
        );
    }

    /// A fill on one connection's order must not reach another connection.
    ///
    /// The whole point of the attribution in `deliver`: before it, every bound
    /// lane received every sweep-produced frame, so an account was told about
    /// orders it never placed. Reverting the filter makes the second assertion
    /// fail - the unrelated connection receives the fill.
    #[tokio::test(flavor = "current_thread")]
    async fn a_swept_fill_reaches_only_the_connection_that_submitted_the_order() {
        let run = crate::run::test_run();
        // Admitted through the real path, because a lane only becomes
        // deliverable once its connection has been committed and reaches its
        // reading boundary.
        let mine_account = AccountId::parse("MOGWAI-001").unwrap();
        let theirs_account = AccountId::parse("MOGWAI-002").unwrap();
        let (_mine_attach, mine_conn) = crate::run::admit_for_test(&run, &mine_account, None);
        let (_theirs_attach, theirs_conn) = crate::run::admit_for_test(&run, &theirs_account, None);
        let (mine, mut my_rx) = ExecLanes::detached_as(mine_conn.connection_id);
        let (theirs, mut their_rx) = ExecLanes::detached_as(theirs_conn.connection_id);
        run.bind_lanes(mine.clone(), "MOGWAI-001", None);
        run.bind_lanes(theirs.clone(), "MOGWAI-002", None);
        let order: mogwai_protocol::VenueOrderId = "V-1".into();
        run.claim_order(order.clone(), "MOGWAI-001");

        // A partial fill, so the batch does not also retire the claim it is
        // being attributed by; the assertion is about delivery, not cleanup.
        let events = vec![VenueMessage::OrderFilled(mogwai_protocol::OrderFilled {
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

    /// A venue-originated order's fill reaches only the account whose ledger
    /// produced it - nobody submitted it, so nothing has claimed it before the
    /// sweep delivers it.
    ///
    /// This was the one deliberate hole in the invisibility property: a
    /// liquidation order the venue mints has no submitting connection, the
    /// ownership table missed, and the fill broadcast to every lane. Ruled
    /// closed 2026-08-20. `deliver_produced` is what the sweep's production
    /// sites call, so this test drives the same fused claim-then-deliver path
    /// and must not claim by hand - the absence of a hand claim is the case
    /// under test, and reverting the claim inside `deliver_produced` makes the
    /// stranger's assertion fail.
    #[tokio::test(flavor = "current_thread")]
    async fn a_venue_originated_fill_reaches_only_the_account_that_produced_it() {
        let run = crate::run::test_run();
        // Admitted through the real path, because a lane only becomes
        // deliverable once its connection has been committed and reaches its
        // reading boundary.
        let mine_account = AccountId::parse("MOGWAI-001").unwrap();
        let theirs_account = AccountId::parse("MOGWAI-002").unwrap();
        let (_mine_attach, mine_conn) = crate::run::admit_for_test(&run, &mine_account, None);
        let (_theirs_attach, theirs_conn) = crate::run::admit_for_test(&run, &theirs_account, None);
        let (mine, mut my_rx) = ExecLanes::detached_as(mine_conn.connection_id);
        let (theirs, mut their_rx) = ExecLanes::detached_as(theirs_conn.connection_id);
        run.bind_lanes(mine.clone(), "MOGWAI-001", None);
        run.bind_lanes(theirs.clone(), "MOGWAI-002", None);

        // The shape a risk flatten produces: a venue-minted id under the
        // reserved prefix, never seen by any dispatcher.
        let events = vec![VenueMessage::OrderFilled(mogwai_protocol::OrderFilled {
            client_order_id: "RISK-BTCUSDT-1".into(),
            venue_order_id: "V-9".into(),
            trade_id: "T-9".into(),
            symbol: mogwai_protocol::Symbol::from("BTCUSDT"),
            position_id: None,
            side: Side::Sell,
            last_qty: Decimal::ONE,
            last_px: Decimal::from(100),
            leaves_qty: Decimal::ZERO,
            commission: Decimal::ZERO,
            commission_currency: "USDT".into(),
            liquidity_side: mogwai_protocol::LiquiditySide::Taker,
            ts_event: 1,
        })];
        deliver_produced(
            &run,
            "MOGWAI-001",
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
            1,
            1,
        );

        assert!(
            my_rx.held_rx.try_recv().is_ok(),
            "the liquidated account must receive the fill that closed its position"
        );
        assert!(
            their_rx.held_rx.try_recv().is_err(),
            "a venue-originated fill must not reach an account it does not concern"
        );
        assert_eq!(
            run.order_owner(&"V-9".into()).as_deref(),
            Some("MOGWAI-001"),
            "production must claim the venue-minted order, so later fills and query rows \
             stay attributed to the account it acted on"
        );
    }

    /// An account snapshot reaches only the connections bound to that account.
    ///
    /// The residual half of the same attribution: `AccountState` names no order,
    /// so order-only attribution read it as venue-wide and fanned one
    /// account's balances and positions to every other account. The sweep
    /// takes a pass per account, so this happened on every pass rather than at
    /// some edge. Asserting on the frame rather than on receipt is the point -
    /// the wrong lane receiving something is only a bug because of what the
    /// something is.
    #[tokio::test(flavor = "current_thread")]
    async fn a_swept_account_snapshot_reaches_only_that_account() {
        let run = crate::run::test_run();
        // Admitted through the real path, because a lane only becomes
        // deliverable once its connection has been committed and reaches its
        // reading boundary.
        let mine_account = AccountId::parse("MOGWAI-001").unwrap();
        let theirs_account = AccountId::parse("MOGWAI-002").unwrap();
        let (_mine_attach, mine_conn) = crate::run::admit_for_test(&run, &mine_account, None);
        let (_theirs_attach, theirs_conn) = crate::run::admit_for_test(&run, &theirs_account, None);
        let (mine, mut my_rx) = ExecLanes::detached_as(mine_conn.connection_id);
        let (theirs, mut their_rx) = ExecLanes::detached_as(theirs_conn.connection_id);
        run.bind_lanes(mine.clone(), "MOGWAI-001", None);
        run.bind_lanes(theirs.clone(), "MOGWAI-002", None);

        let events = vec![VenueMessage::AccountState(mogwai_protocol::AccountState {
            account_id: AccountId::parse("MOGWAI-002").expect("a legal account label"),
            balances: vec![mogwai_protocol::Balance {
                currency: "USDT".into(),
                total: Decimal::from(100_000),
                free: Decimal::from(100_000),
                locked: Decimal::ZERO,
            }],
            positions: Vec::new(),
            margins: Vec::new(),
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
            their_rx.held_rx.try_recv().is_ok(),
            "the account the snapshot is about must receive it"
        );
        assert!(
            my_rx.held_rx.try_recv().is_err(),
            "a connection on another account must not receive its balances"
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

    /// The destructive-read half of the same guard: a pass that abandons its
    /// interval must not have consumed that interval's price extremes.
    ///
    /// `take` is a consuming read, so taking it before the reading was checked
    /// left the high and the low of an interval the venue then re-swept gone
    /// permanently - the account marked over a span whose extremes had been
    /// dropped, which is the hole `extremes.rs` exists to close.
    #[test]
    fn an_unread_pass_leaves_its_price_extremes_owed() {
        let extremes = crate::extremes::PriceExtremes::default();
        let mut writer = crate::extremes::SpanWriter::default();
        extremes.record(&mut writer, rust_decimal::Decimal::from(140), 1);
        extremes.record(&mut writer, rust_decimal::Decimal::from(90), 2);

        assert!(
            commit_pass(None, &extremes, false).is_none(),
            "a failed reading abandons the pass"
        );

        // The next pass re-sweeps the same interval, and must still find the
        // spike the tape actually printed in it.
        let (_reads, span) = commit_pass(Some(((Vec::new(), Vec::new()), None)), &extremes, false)
            .expect("the reading succeeded");
        let span = span.expect("the tape printed in this interval");
        assert_eq!(span.high_px, rust_decimal::Decimal::from(140));
        assert_eq!(span.low_px, rust_decimal::Decimal::from(90));

        // And a committed pass does consume it, so the next span starts from
        // its own prints rather than re-ratcheting a peak already spent.
        let (_reads, span) = commit_pass(Some(((Vec::new(), Vec::new()), None)), &extremes, false)
            .expect("the reading succeeded");
        assert!(span.is_none(), "a committed pass consumes the span");
    }

    /// The settlement half of the frontier guard, at the layer that decides it.
    ///
    /// An unreadable ordinary mark is dropped and the pass proceeds; an
    /// unreadable settlement price refuses the whole read, which is what makes
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
                breach_action: MarginBreachAction::Refuse,
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
            limit_offset: None,
            reduce_only: false,
            post_only: false,
            time_in_force: TimeInForce::Gtc,
            expire_time: None,
            link: None,
        };
        engine.process_with_market(
            Command::SubmitOrder(order),
            1,
            Some(MarketReading {
                last_px: Decimal::from(21_000),
                ts_ns: 1,
                band_ticks: 0,
            }),
        );
        engine
    }

    fn engine_with_perpetual_position() -> mogwai_engine::Engine {
        let def = InstrumentDef {
            symbol: "BTCUSDT.P".into(),
            class: InstrumentClass::Perpetual {
                underlying: "BTC".into(),
                settlement_currency: "USDT".into(),
                multiplier: Decimal::ONE,
                asset_class: WireAssetClass::Cryptocurrency,
                funding_interval_ns: 8 * 3_600 * 1_000_000_000,
                funding_rate: Decimal::new(1, 4),
                index_symbol: None,
                funding_clamp: Decimal::ZERO,
            },
            price_precision: 2,
            size_precision: 0,
            price_increment: Decimal::new(1, 2),
            size_increment: Decimal::ONE,
        };
        let mut engine = mogwai_engine::Engine::build(EngineConfig {
            account_id: AccountId::parse("TEST-001").unwrap(),
            instruments: vec![def],
            balances: HashMap::from([("USDT".into(), Decimal::from(100_000))]),
            fill_seed: 7,
        });
        engine.set_margin_policy(
            "BTCUSDT.P".into(),
            MarginPolicy {
                initial_per_contract: Decimal::ZERO,
                maintenance_per_contract: Decimal::ZERO,
                breach_action: MarginBreachAction::Refuse,
                basis: Default::default(),
            },
        );
        engine.process(
            Command::SubmitOrder(SubmitOrder {
                client_order_id: "OPEN-PERP".into(),
                symbol: "BTCUSDT.P".into(),
                position_id: None,
                side: Side::Buy,
                order_type: OrderType::Market,
                quantity: Decimal::ONE,
                price: Some(Decimal::from(50_000)),
                trigger_price: None,
                trail_offset: None,
                limit_offset: None,
                reduce_only: false,
                post_only: false,
                time_in_force: TimeInForce::Gtc,
                expire_time: None,
                link: None,
            }),
            1,
        );
        engine
    }

    /// The account for the tick-resolution tests: long one MNQ at 21,000 under
    /// a trailing drawdown, on a venue whose engine already holds the position.
    async fn policed_account_with_action(
        run: &Arc<crate::run::Run>,
        amount: u64,
        action: mogwai_protocol::risk::BreachAction,
    ) -> Arc<crate::run::Account> {
        let account = AccountId::parse("RISK-001").unwrap();
        run.open_account(
            &account,
            HashMap::from([("USD".to_string(), Decimal::from(10_000))]),
            mogwai_protocol::risk::AccountPolicy {
                trailing_drawdown: Some(mogwai_protocol::risk::TrailingDrawdown {
                    amount: Decimal::from(amount),
                    basis: mogwai_protocol::risk::TrailingBasis::PeakEquity,
                    lock_at_equity: None,
                    on_breach: action,
                }),
                daily_loss_limit: None,
                reset_minute_utc: 0,
                currency: Some("USD".to_owned()),
                ..mogwai_protocol::risk::AccountPolicy::default()
            },
        )
        .expect("a fresh account opens");
        let account_state = run.account(&account);
        *account_state.engine.lock().await = engine_with_position();
        account_state
    }

    async fn policed_account(run: &Arc<crate::run::Run>, amount: u64) -> Arc<crate::run::Account> {
        policed_account_with_action(run, amount, mogwai_protocol::risk::BreachAction::Terminate)
            .await
    }

    /// The gap this closes. A spike that opened and closed entirely between two
    /// sweep passes used to be invisible: the pass saw only the closing mark, so
    /// the account kept drawdown room it had actually spent. The span carries
    /// the high, so the ratchet sees it.
    ///
    /// One MNQ long at 21,000 with a 2 multiplier: a 100-point spike is 200
    /// dollars of equity, so a 500-dollar trail that ratcheted spends 200 of it.
    #[tokio::test(flavor = "current_thread")]
    async fn a_spike_between_two_passes_spends_drawdown_budget() {
        let run = crate::run::test_run();
        let account_state = policed_account(&run, 500).await;
        let mut engine = account_state.engine.lock().await;
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
            &account_state,
            &mut engine,
            &mut events,
            &symbol,
            Some(span),
            10,
            0,
            0,
        );
        assert!(!terminated, "the spike breaches nothing on its own");
        let state = account_state
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

    /// The other half: a collapse that recovered before the pass. A
    /// mark-cadence evaluation sees an account comfortably inside its floor; the
    /// account was liquidated at the venue being modelled.
    #[tokio::test(flavor = "current_thread")]
    async fn a_collapse_that_recovered_before_the_pass_still_breaches() {
        let run = crate::run::test_run();
        let account_state = policed_account(&run, 500).await;
        let mut engine = account_state.engine.lock().await;
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
            &account_state,
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
            account_state.risk.lock().unwrap().is_locked(),
            "a terminating breach locks the account"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_lock_breach_still_observes_the_closing_equity() {
        let run = crate::run::test_run();
        let account_state = policed_account_with_action(
            &run,
            500,
            mogwai_protocol::risk::BreachAction::LockUntilReset,
        )
        .await;
        let mut engine = account_state.engine.lock().await;
        let mut events = Vec::new();
        let symbol = mogwai_protocol::Symbol::from("MNQ");
        engine.mark(
            &[(
                mogwai_protocol::Symbol::clone(&symbol),
                Decimal::from(21_200),
            )],
            10,
        );
        let span = crate::extremes::PriceSpan {
            high_px: Decimal::from(21_200),
            high_ns: 9,
            low_px: Decimal::from(20_600),
            low_ns: 5,
        };
        let (_, _, terminated) = enforce_policy(
            &account_state,
            &mut engine,
            &mut events,
            &symbol,
            Some(span),
            10,
            0,
            0,
        );
        assert!(!terminated, "a lock breach does not terminate the account");
        let state = account_state
            .risk
            .lock()
            .unwrap()
            .state(engine.valuation_in("USD").expect("valuable"));
        assert_eq!(
            state.peak_equity,
            Decimal::from(10_400),
            "the close ratchets the peak even after the earlier reading locks"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn one_unvaluable_extreme_refuses_the_whole_policy_span() {
        let run = crate::run::test_run();
        let account_state = policed_account(&run, 500).await;
        let mut engine = account_state.engine.lock().await;
        let mut events = Vec::new();
        let symbol = mogwai_protocol::Symbol::from("MNQ");
        engine.mark(
            &[(
                mogwai_protocol::Symbol::clone(&symbol),
                Decimal::from(21_000),
            )],
            10,
        );
        let before = account_state
            .risk
            .lock()
            .unwrap()
            .state(engine.valuation_in("USD").expect("valuable"));
        let span = crate::extremes::PriceSpan {
            high_px: Decimal::MAX,
            high_ns: 9,
            low_px: Decimal::from(21_100),
            low_ns: 5,
        };
        let (_, _, terminated) = enforce_policy(
            &account_state,
            &mut engine,
            &mut events,
            &symbol,
            Some(span),
            10,
            0,
            0,
        );
        assert!(!terminated);
        let after = account_state
            .risk
            .lock()
            .unwrap()
            .state(engine.valuation_in("USD").expect("valuable"));
        assert_eq!(
            after.peak_equity, before.peak_equity,
            "no reading from a partly unvaluable span may enter the ledger"
        );
    }

    /// A trailing stop follows the span's high rather than its closing mark,
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
            limit_offset: None,
            reduce_only: true,
            post_only: false,
            time_in_force: TimeInForce::Gtc,
            expire_time: None,
            link: None,
        };
        engine.process(Command::SubmitOrder(trail), 2);
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
                .any(|event| matches!(event, VenueMessage::AccountState(_)))
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
                .filter(|event| matches!(event, VenueMessage::AccountState(_)))
                .count(),
            1
        );
    }

    #[test]
    fn a_pass_snapshot_is_taken_after_its_funding_payment() {
        const FUNDING: u64 = 8 * 3_600 * 1_000_000_000;
        let mut engine = engine_with_perpetual_position();
        let (events, _, _) = apply_engine_pass_on_clock(
            &mut engine,
            &[],
            Vec::new(),
            &[("BTCUSDT.P".into(), Decimal::from(60_000))],
            &[],
            FUNDING - 1,
            FUNDING,
            None,
            mogwai_protocol::SimClock::identity(),
        );
        let delivered = events.iter().find_map(|event| match event {
            VenueMessage::AccountState(state) => Some(state),
            _ => None,
        });
        assert_eq!(
            serde_json::to_value(delivered.expect("the pass emitted its account state")).unwrap(),
            serde_json::to_value(engine.account_snapshot(FUNDING)).unwrap(),
            "the delivered state must include the funding payment"
        );
    }

    #[test]
    fn an_unpaced_pass_uses_its_clock_bounded_span() {
        let extremes = crate::extremes::PriceExtremes::default();
        let mut writer = crate::extremes::SpanWriter::default();
        extremes.record(&mut writer, Decimal::from(999), 999);
        let bounded = crate::extremes::PriceSpan::of(Decimal::from(100), 5);
        let (_, span) = commit_pass(
            Some(((Vec::new(), Vec::new()), Some(bounded))),
            &extremes,
            true,
        )
        .expect("the market reads succeeded");
        assert_eq!(span, Some(bounded));
        assert!(
            extremes.take().is_some(),
            "the future publisher span was not consumed as this pass's prices"
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
                VenueMessage::AccountState(state) => Some(state),
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
            limit_offset: None,
            reduce_only: true,
            post_only: false,
            time_in_force: TimeInForce::Gtc,
            expire_time: None,
            link: None,
        };
        engine.process(Command::SubmitOrder(order), 2);
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
                .any(|event| matches!(event, VenueMessage::OrderFilled(_)))
        );
        // Survives, which is the half the fill assertion below cannot see: a
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
                .any(|event| matches!(event, VenueMessage::OrderFilled(_)))
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

        // A closure is an empty mark set: there is no tape inside it, so the
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
