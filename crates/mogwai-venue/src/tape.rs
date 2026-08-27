// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! One boat-owned paced tape and its bounded broadcast fanout.

use crate::config::{now_ns, sim_now_ns};
use mogwai_data::TickFault;
use mogwai_protocol::{FundingTerms, SimClock, Symbol, VenueMessage};
use rust_decimal::Decimal;
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
    last_funding: Mutex<Option<TapeFrame>>,
    cancel: Arc<AtomicBool>,
    alive: AtomicBool,
    fault: Mutex<Option<TickFault>>,
}
pub(crate) struct TapeSpawn {
    pub(crate) sim: SimClock,
    pub(crate) speed: f64,
    pub(crate) fanout_depth: usize,
    pub(crate) fault_tx: mpsc::Sender<TickFault>,
    /// Where this thread records the high and low of the span since the sweeper
    /// last looked. See `crate::extremes`: it is what gives peak equity and a
    /// trailing stop tick resolution without evaluating either here.
    pub(crate) extremes: Arc<crate::extremes::PriceExtremes>,
    /// The resident trailing window the submit path's market reading is served
    /// from. Folded at pull time, before the pacing sleep, because the pull is
    /// what proves every earlier instant complete; see `crate::vol_window` for
    /// why that leaks no lookahead.
    pub(crate) vol_window: Arc<crate::vol_window::VolWindow>,
    /// The funding terms of this boat's river, resolved at placement, plus the
    /// registry handle an index read needs. `None` for a river whose class
    /// exchanges no funding, whose interval is zero, or whose profile could not
    /// be resolved - which prices every non-funding boat at one option check
    /// per tick.
    pub(crate) funding: Option<TapeFunding>,
}

/// What the tape thread needs to price a funding instant of its own river.
pub(crate) struct TapeFunding {
    pub(crate) symbol: Symbol,
    pub(crate) terms: FundingTerms,
    /// `crate::source::Rivers`, the river store - not the connection registry
    /// in `registry.rs`. An index read goes through this.
    pub(crate) rivers: Arc<crate::source::Rivers>,
}
impl Tape {
    pub(crate) fn start(
        mut cursor: Box<dyn mogwai_data::TickSource + Send>,
        spawn: TapeSpawn,
    ) -> (Arc<Self>, thread::JoinHandle<()>) {
        let (tx, _) = broadcast::channel(spawn.fanout_depth);
        let tape = Arc::new(Self {
            tx,
            last_quote: Mutex::new(None),
            last_funding: Mutex::new(None),
            cancel: Arc::new(AtomicBool::new(false)),
            alive: AtomicBool::new(true),
            fault: Mutex::new(None),
        });
        let worker = Arc::clone(&tape);
        let handle = thread::spawn(move || {
            let wall_anchor = now_ns();
            let instant_anchor = Instant::now();
            // This thread's own running extremes. Stack-local on purpose: the
            // comparison is per tick and must not take a lock, so the shared
            // slot is written only when an extreme actually moves.
            let mut span = crate::extremes::SpanWriter::default();
            // Ticks published since this thread last offered the host a
            // scheduling point. Only an unpaced tape uses it; a paced one sleeps
            // to a wall deadline and yields by doing so.
            let mut since_yield: u32 = 0;
            let mut last_mark: Option<Decimal> = None;
            let mut prev_ts: Option<u64> = None;
            while !worker.cancel.load(Ordering::Relaxed) {
                let Some(tick) = cursor.next_tick() else {
                    if let (Some(funding), Some(mark), Some(prev)) =
                        (&spawn.funding, last_mark, prev_ts)
                    {
                        publish_funding_span(&worker, funding, prev, sim_now_ns(spawn.sim), mark);
                    }
                    if let Some(fault) = cursor.fault() {
                        *worker
                            .fault
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(fault);
                        tracing::error!(?fault, "tape source faulted");
                        let _fault_receiver_gone = spawn.fault_tx.send(fault);
                    }
                    spawn.vol_window.close();
                    break;
                };
                match &tick {
                    mogwai_data::TickEvent::Trade(trade) => {
                        spawn
                            .vol_window
                            .fold(trade.ts_event, Some(trade.price), None);
                    }
                    mogwai_data::TickEvent::Quote(quote) => {
                        spawn.vol_window.fold(
                            quote.ts_event,
                            None,
                            Some(mogwai_data::BookState {
                                bid_px: quote.bid_px,
                                ask_px: quote.ask_px,
                                bid_sz: quote.bid_sz,
                                ask_sz: quote.ask_sz,
                                ts_ns: quote.ts_event,
                            }),
                        );
                    }
                }
                pace(
                    &worker,
                    &spawn,
                    tick.ts_event(),
                    wall_anchor,
                    instant_anchor,
                    &mut since_yield,
                );
                if worker.cancel.load(Ordering::Relaxed) {
                    break;
                }
                let ts_event = tick.ts_event();
                if let (Some(funding), Some(mark), Some(prev)) =
                    (&spawn.funding, last_mark, prev_ts)
                {
                    publish_funding_span(&worker, funding, prev, ts_event, mark);
                }
                // Trades only, and that is the same rule the mark reads follow:
                // a mark is a last-print read, so an extreme drawn from quotes
                // would be an extreme no fill or valuation could ever have been
                // taken at. Recorded before publication, so a reader that takes
                // the span after seeing a frame cannot miss that frame's price.
                if spawn.speed != 0.0
                    && let mogwai_data::TickEvent::Trade(trade) = &tick
                {
                    spawn.extremes.record(&mut span, trade.price, ts_event);
                }
                let is_quote = matches!(tick, mogwai_data::TickEvent::Quote(_));
                let next_mark = match &tick {
                    mogwai_data::TickEvent::Trade(trade) => Some(trade.price),
                    mogwai_data::TickEvent::Quote(_) => None,
                };
                let event = match tick {
                    mogwai_data::TickEvent::Trade(trade) => {
                        mogwai_protocol::VenueMessage::Trade(trade)
                    }
                    mogwai_data::TickEvent::Quote(quote) => {
                        mogwai_protocol::VenueMessage::Quote(quote)
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
                if let Some(mark) = next_mark {
                    last_mark = Some(mark);
                }
                prev_ts = Some(ts_event);
            }
            worker.alive.store(false, Ordering::Release);
        });
        (tape, handle)
    }
    /// Subscribe, plus whichever of the last quote and the last funding rate
    /// this boat has published, in `ts_event` order. Either may be absent and
    /// that absence is the contract, not an implementation detail: a socket
    /// binding between a boat's first trade and its first quote is handed no
    /// quote and therefore sees a trade as its first market frame, and a boat
    /// that has crossed no funding instant has no rate to replay. Callers must
    /// not turn this into a snapshot-first promise - there is nothing to
    /// snapshot yet, and the tape's own next frame is immediately behind it.
    pub(crate) fn subscribe_with_snapshot(
        &self,
    ) -> (broadcast::Receiver<TapeFrame>, Vec<TapeFrame>) {
        self.subscribe_with_snapshot_inner(|| {})
    }
    fn subscribe_with_snapshot_inner(
        &self,
        after_subscribe: impl FnOnce(),
    ) -> (broadcast::Receiver<TapeFrame>, Vec<TapeFrame>) {
        // Both slots are read under their locks and the subscription is taken
        // while they are held, so a frame published concurrently is either in
        // the snapshot or in the receiver, never both and never neither. Taking
        // either lock after subscribing would duplicate that frame.
        let quote = self
            .last_quote
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let funding = self
            .last_funding
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let receiver = self.tx.subscribe();
        after_subscribe();
        let mut snapshots: Vec<_> = [quote.clone(), funding.clone()]
            .into_iter()
            .flatten()
            .collect();
        snapshots.sort_by_key(|frame| frame.ts_event);
        (receiver, snapshots)
    }
    /// The funding twin of `publish`. It deliberately does not touch
    /// `last_quote`: a funding frame is not a quote and must not stand in for
    /// the book a reconnecting socket replays.
    fn publish_funding(&self, frame: TapeFrame) {
        *self
            .last_funding
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(frame.clone());
        drop(self.tx.send(frame));
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
    pub(crate) fn fault(&self) -> Option<TickFault> {
        *self
            .fault
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    pub(crate) fn cancel_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel)
    }
    /// False once the pacing thread has left its loop. The one observable a
    /// wind-down test can assert on without reaching into the worker handle
    /// the ticket's drop already took.
    #[cfg(test)]
    pub(crate) fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }
}

/// The funding instants the half-open span `(from_ns, to_ns]` crosses, in order.
///
/// The one enumerator, shared by the publisher below and by the sweeper's
/// funding-observation walk: the ledger charges exactly the instants the tape
/// publishes because both sides ask this function, not because two copies of
/// the arithmetic happen to agree.
///
/// Instants sit on multiples of `interval_ns` from the unix epoch. A zero
/// `interval_ns` and an empty or reversed span both yield nothing.
pub(crate) fn funding_instants(
    from_ns: u64,
    to_ns: u64,
    interval_ns: u64,
) -> impl Iterator<Item = u64> {
    // An empty inclusive range, spelled so the zero-interval division never
    // happens rather than guarded twice.
    let (start, end) = if interval_ns == 0 || to_ns <= from_ns {
        (1, 0)
    } else {
        (from_ns / interval_ns + 1, to_ns / interval_ns)
    };
    (start..=end).map(move |multiple| multiple * interval_ns)
}

/// Publish one `FundingRate` frame per instant the span `(from_ns, to_ns]`
/// crosses, each priced at `mark` - the boat's last published trade price.
///
/// The index is resolved at most once per span, and only when the span actually
/// crosses an instant: `materialized_symbols` takes a mutex and allocates every
/// river name, which is the same reason the sweeper guards its own call on the
/// boat naming an index at all. Resolving it per instant, or per tick, would put
/// that cost on the tape's hot path.
fn publish_funding_span(
    tape: &Tape,
    funding: &TapeFunding,
    from_ns: u64,
    to_ns: u64,
    mark: Decimal,
) {
    // `None` until the first instant asks; `Some(None)` once asked and refused.
    let mut index_key: Option<Option<crate::source::RiverKey>> = None;
    for instant in funding_instants(from_ns, to_ns, funding.terms.interval_ns) {
        let key = index_key.get_or_insert_with(|| {
            let symbol = funding.terms.index_symbol.as_deref()?;
            // The sweeper's exact gate: reading an index must never spend a
            // river nobody asked for, because `last_trade_at_or_before`
            // materializes. An unmaterialized index leaves the premium at zero.
            funding
                .rivers
                .materialized_symbols()
                .iter()
                .any(|existing| existing == symbol)
                .then(|| funding.rivers.key_for_symbol(symbol).ok())
                .flatten()
        });
        let index = key
            .as_ref()
            .and_then(|key| crate::fills::read_last(key, instant, &funding.rivers));
        let event = VenueMessage::FundingRate {
            symbol: Arc::clone(&funding.symbol),
            rate: funding.terms.rate(mark, index),
            interval_ns: funding.terms.interval_ns,
            next_funding_ns: instant.saturating_add(funding.terms.interval_ns),
            ts_event: instant,
        };
        match serde_json::to_string(&event) {
            Ok(payload) => tape.publish_funding(TapeFrame {
                payload: Arc::from(payload),
                ts_event: instant,
            }),
            Err(error) => tracing::error!(
                symbol = %funding.symbol,
                instant,
                %error,
                "dropping a funding frame that could not be serialized"
            ),
        }
    }
}

/// Maximum wall time one slice of a pacing sleep runs before the tape thread
/// re-checks its cancel flag, so a stopping venue is not parked for a whole
/// inter-tick gap.
const TAPE_SLEEP_POLL: Duration = Duration::from_millis(20);

/// Pace one tick against the run clock, or, for a firehose, yield to the host
/// often enough that one river cannot capture a core.
fn pace(
    tape: &Tape,
    spawn: &TapeSpawn,
    ts: u64,
    wall_anchor: u64,
    instant_anchor: Instant,
    since_yield: &mut u32,
) {
    if spawn.speed == 0.0 {
        *since_yield += 1;
        if *since_yield >= UNPACED_YIELD_TICKS {
            *since_yield = 0;
            thread::yield_now();
        }
        return;
    }
    sleep_until_wall_cancellable(tape, spawn.sim.wall_ns(ts), wall_anchor, instant_anchor);
}

/// How many ticks an unpaced tape publishes between scheduling points.
///
/// Receiver-blind, and that is the point. An unpaced tape used to park while the
/// ring was more than half full, which made the slowest subscriber the thing
/// that decided when a shared boat published. That was tolerable only while a
/// lagging passenger was ejected: one dead consumer cost one stall and then
/// left. A declared hole is no longer fatal, so the ejection is gone - and a
/// passenger whose sustainable read rate is below the publish rate would then
/// have parked the boat every stall interval for the whole run, imposing its own
/// slowness on every other account sharing that hull. That is exactly the
/// non-interference passengers of different accounts are owed.
///
/// What replaces it depends on production work alone, never on ring occupancy
/// or the slowest cursor: a fixed cadence bounds how long one unpaced river can
/// hold a core, without letting any consumer influence the tape. The ring is now
/// the only delivery slack a passenger has, and overrunning it is declared
/// rather than waited out.
const UNPACED_YIELD_TICKS: u32 = 64;

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
            last_funding: Mutex::new(None),
            cancel: Arc::new(AtomicBool::new(false)),
            alive: AtomicBool::new(true),
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
        let snapshot = snapshot.into_iter().next().unwrap();
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
            last_funding: Mutex::new(None),
            cancel: Arc::new(AtomicBool::new(false)),
            alive: AtomicBool::new(true),
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
        let snapshot = snapshot.into_iter().next().expect("current BBO snapshot");
        let trade = receiver.try_recv().expect("queued trade");
        assert_eq!(snapshot.payload.as_ref(), "quote");
        assert_eq!(trade.payload.as_ref(), "trade");
        assert!(snapshot.ts_event <= trade.ts_event);
    }

    /// The one enumerator's semantics, pinned by explicit expectation. The
    /// ledger's funding observations and the published frames both come
    /// through this function, so this table is the schedule contract for both.
    #[test]
    fn funding_instants_enumerate_the_half_open_span() {
        let cases: [(u64, u64, u64, &[u64]); 9] = [
            // Zero interval, a span crossing none, a span crossing exactly one,
            // both ends on instants, an abutting span, a span crossing many,
            // and an empty span.
            (0, 10, 0, &[]),
            (0, 9, 10, &[]),
            (0, 10, 10, &[10]),
            (10, 20, 10, &[20]),
            (20, 30, 10, &[30]),
            (9, 95, 10, &[10, 20, 30, 40, 50, 60, 70, 80, 90]),
            (10, 10, 10, &[]),
            (95, 9, 10, &[]),
            (0, u64::MAX, u64::MAX, &[u64::MAX]),
        ];
        for (from, to, interval, expected) in cases {
            let actual: Vec<_> = funding_instants(from, to, interval).collect();
            assert_eq!(
                actual, expected,
                "the instants over ({from}, {to}] at {interval}"
            );
            for instant in &actual {
                assert!(
                    *instant > from && *instant <= to,
                    "instant {instant} is outside the span"
                );
                assert_eq!(
                    *instant % interval,
                    0,
                    "instant {instant} is not epoch-aligned"
                );
            }
        }
    }

    #[test]
    fn funding_snapshot_is_replayed_in_event_order() {
        let (tx, _) = broadcast::channel(16);
        let tape = Tape {
            tx,
            last_quote: Mutex::new(None),
            last_funding: Mutex::new(None),
            cancel: Arc::new(AtomicBool::new(false)),
            alive: AtomicBool::new(true),
            fault: Mutex::new(None),
        };
        tape.publish_funding(TapeFrame {
            payload: Arc::from("funding"),
            ts_event: 10,
        });
        tape.publish(
            TapeFrame {
                payload: Arc::from("quote"),
                ts_event: 11,
            },
            true,
        );
        let (_, snapshots) = tape.subscribe_with_snapshot();
        assert_eq!(snapshots.len(), 2);
        assert_eq!(snapshots[0].payload.as_ref(), "funding");
        assert_eq!(snapshots[1].payload.as_ref(), "quote");
    }

    /// A boat that has crossed no instant replays no rate, which is the
    /// contract `subscribe_with_snapshot`'s doc comment states and not a gap.
    #[test]
    fn a_boat_that_crossed_no_instant_replays_no_funding_rate() {
        let (tx, _) = broadcast::channel(16);
        let tape = Tape {
            tx,
            last_quote: Mutex::new(None),
            last_funding: Mutex::new(None),
            cancel: Arc::new(AtomicBool::new(false)),
            alive: AtomicBool::new(true),
            fault: Mutex::new(None),
        };
        let (_, snapshots) = tape.subscribe_with_snapshot();
        assert!(snapshots.is_empty());
    }
}

/// The tape thread's funding emission, driven through `Tape::start` with a
/// scripted cursor so the loop's own ordering - after pacing, before the
/// crossing tick - is what is under test rather than the enumerator alone.
#[cfg(test)]
mod funding_emission_tests {
    use super::*;
    use mogwai_data::{MemorySource, TickEvent, TickSource};
    use mogwai_protocol::{AggressorSide, QuoteTick, TradeTick};

    const INTERVAL: u64 = 28_800_000_000_000;
    /// One minute, for the tests that actually read the index river. An instant
    /// is where that read happens, so a short interval keeps the walk from the
    /// tape origin short; the tests that never touch the registry use the real
    /// eight-hour cycle.
    const INDEX_INTERVAL: u64 = 60_000_000_000;
    const SYMBOL: &str = "BTCUSDT";
    const INDEX_SYMBOL: &str = "SECOND";

    /// A source that will not yield its first tick until the test has attached
    /// a receiver. Without it the tape thread can drain and exit before
    /// `subscribe` runs, and a broadcast receiver never sees what was sent
    /// before it existed - so the test would pass or fail on a race.
    struct GatedSource {
        gate: Option<std::sync::mpsc::Receiver<()>>,
        inner: MemorySource,
    }

    impl TickSource for GatedSource {
        fn next_tick(&mut self) -> Option<TickEvent> {
            if let Some(gate) = self.gate.take() {
                let _opened = gate.recv();
            }
            self.inner.next_tick()
        }
    }

    fn trade(price: i64, ts_event: u64) -> TickEvent {
        TickEvent::Trade(TradeTick {
            symbol: SYMBOL.into(),
            price: Decimal::from(price),
            size: Decimal::ONE,
            aggressor: AggressorSide::Buyer,
            ts_event,
        })
    }

    fn quote(ts_event: u64) -> TickEvent {
        TickEvent::Quote(QuoteTick {
            symbol: SYMBOL.into(),
            bid_px: Decimal::from(99),
            ask_px: Decimal::from(101),
            bid_sz: Decimal::ONE,
            ask_sz: Decimal::ONE,
            ts_event,
        })
    }

    fn terms(index_symbol: Option<&str>) -> FundingTerms {
        terms_over(INTERVAL, index_symbol)
    }

    /// The clamp is zero throughout: a clamp that bit would flatten distinct
    /// premiums onto one number and make the index and divergence assertions
    /// pass for the wrong reason.
    fn terms_over(interval_ns: u64, index_symbol: Option<&str>) -> FundingTerms {
        FundingTerms {
            interval_ns,
            interest: Decimal::new(1, 4),
            index_symbol: index_symbol.map(str::to_owned),
            clamp: Decimal::ZERO,
        }
    }

    /// A registry whose index river has been materialized by reading it, which
    /// is what any other consumer of that river would do.
    fn materialized_index_rivers() -> Arc<crate::source::Rivers> {
        let rivers = crate::fills::test_rivers_with_a_second_symbol();
        let key = rivers
            .key_for_symbol(INDEX_SYMBOL)
            .expect("the index river resolves");
        let _priced = rivers
            .last_trade_at_or_before(&key, INDEX_INTERVAL)
            .expect("the index river reads");
        assert!(
            rivers
                .materialized_symbols()
                .iter()
                .any(|symbol| symbol == INDEX_SYMBOL)
        );
        rivers
    }

    /// The index price the publisher will read at `ts`, read the same way.
    fn index_at(rivers: &crate::source::Rivers, ts: u64) -> Option<Decimal> {
        let key = rivers
            .key_for_symbol(INDEX_SYMBOL)
            .expect("the index river resolves");
        let index = rivers
            .last_trade_at_or_before(&key, ts)
            .expect("the index river reads");
        assert!(
            index.is_some_and(|price| !price.is_zero()),
            "an absent or zero index leaves the premium at zero, which would make \
             every index assertion here vacuous"
        );
        index
    }

    fn funding(terms: FundingTerms) -> TapeFunding {
        TapeFunding {
            symbol: SYMBOL.into(),
            terms,
            rivers: crate::fills::test_rivers_with_a_second_symbol(),
        }
    }

    /// Every frame the tape published, in publication order, decoded.
    fn frames(
        ticks: Vec<TickEvent>,
        funding: Option<TapeFunding>,
        sim_now_ns: u64,
    ) -> Vec<VenueMessage> {
        let (fault_tx, _fault_rx) = mpsc::channel();
        let (open, gate) = std::sync::mpsc::channel();
        let cursor = GatedSource {
            gate: Some(gate),
            inner: MemorySource::new(ticks),
        };
        let (tape, handle) = Tape::start(
            Box::new(cursor),
            TapeSpawn {
                // Speed zero pins the clock at `sim_epoch_ns`, so the
                // exhaustion flush reads exactly what the test asked for
                // instead of whatever the wall clock says.
                sim: SimClock {
                    sim_epoch_ns: sim_now_ns,
                    wall_anchor_ns: 0,
                    speed: 0.0,
                },
                speed: 0.0,
                fanout_depth: 256,
                fault_tx,
                extremes: Arc::new(crate::extremes::PriceExtremes::default()),
                vol_window: Arc::new(crate::vol_window::VolWindow::starting_at(0)),
                funding,
            },
        );
        let mut receiver = tape.tx.subscribe();
        let _opened = open.send(());
        handle.join().expect("tape thread");
        let mut published = Vec::new();
        while let Ok(frame) = receiver.try_recv() {
            published
                .push(serde_json::from_str(frame.payload.as_ref()).expect("every frame reparses"));
        }
        published
    }

    fn rates(published: &[VenueMessage]) -> Vec<(u64, Decimal)> {
        published
            .iter()
            .filter_map(|frame| match frame {
                VenueMessage::FundingRate { rate, ts_event, .. } => Some((*ts_event, *rate)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_crossed_instant_is_published_before_the_tick_that_crossed_it() {
        let published = frames(
            vec![trade(100, INTERVAL - 1), trade(200, INTERVAL + 1)],
            Some(funding(terms(None))),
            0,
        );
        let VenueMessage::FundingRate {
            symbol,
            rate,
            interval_ns,
            next_funding_ns,
            ts_event,
        } = &published[1]
        else {
            panic!("the funding frame must sit between the two trades: {published:?}");
        };
        assert_eq!(symbol.as_ref(), SYMBOL);
        assert_eq!(*ts_event, INTERVAL, "the instant is epoch-aligned");
        assert_eq!(*next_funding_ns, INTERVAL * 2);
        assert_eq!(*interval_ns, INTERVAL);
        // Priced at the mark standing at the instant - the trade before it, not
        // the trade that crossed it.
        assert_eq!(*rate, terms(None).rate(Decimal::from(100), None));
        assert_eq!(rates(&published).len(), 1);
    }

    #[test]
    fn two_instants_are_each_priced_at_the_mark_standing_when_they_are_emitted() {
        let published = frames(
            vec![
                trade(100, 1),
                trade(200, INTERVAL + 1),
                trade(300, INTERVAL * 3 + 1),
            ],
            Some(funding(terms(None))),
            0,
        );
        assert_eq!(
            rates(&published),
            vec![
                (INTERVAL, terms(None).rate(Decimal::from(100), None)),
                (INTERVAL * 2, terms(None).rate(Decimal::from(200), None)),
                (INTERVAL * 3, terms(None).rate(Decimal::from(200), None)),
            ]
        );
    }

    /// The reconciliation pin, replacing the old divergence pin. The ledger's
    /// per-instant funding observations and the published frames price the
    /// same instants through the same enumerator and the same rate rule, so
    /// what an `apply_funding` walk over these observations charges is exactly
    /// the sum the published rates state.
    ///
    /// It needs a materialized index because that is where the old divergence
    /// lived: with no index the premium is zero at every mark, so the rate
    /// does not depend on the mark and the two sides could not disagree. The
    /// moving mark is asserted to actually move the published rate, so a flat
    /// mark fails here rather than passing vacuously.
    #[test]
    fn published_rates_reconstruct_the_charged_rates() {
        let terms = terms_over(INDEX_INTERVAL, Some(INDEX_SYMBOL));
        let rivers = materialized_index_rivers();
        let pass_end = INDEX_INTERVAL * 2 + 1;
        let published = frames(
            vec![
                trade(100, 1),
                trade(200, INDEX_INTERVAL + 1),
                trade(400, pass_end),
            ],
            Some(TapeFunding {
                symbol: SYMBOL.into(),
                terms: terms.clone(),
                rivers: Arc::clone(&rivers),
            }),
            0,
        );
        let rates = rates(&published);
        assert_eq!(
            rates.len(),
            2,
            "the span crosses two instants: {published:?}"
        );
        assert_ne!(
            rates[0].1, rates[1].1,
            "the mark moved between the instants, so the published rates must too"
        );
        // The ledger side of the same instants: one observation per published
        // instant, priced by the same standing-mark rule the publisher used.
        // The marks standing at the two instants are the trades before them.
        for (published_rate, (instant, mark)) in rates
            .iter()
            .zip([(INDEX_INTERVAL, 100_i64), (INDEX_INTERVAL * 2, 200_i64)])
        {
            let charged = terms.rate(Decimal::from(mark), index_at(&rivers, instant));
            assert_eq!(
                published_rate.1, charged,
                "the rate the ledger charges at {instant} is the rate the tape published"
            );
        }
    }

    #[test]
    fn no_instant_is_published_before_the_boats_first_trade() {
        // Quotes carry no mark, so the instant they cross has no price and is
        // deliberately skipped, as is the one the first trade itself crosses.
        let published = frames(
            vec![quote(1), quote(INTERVAL + 1), trade(100, INTERVAL * 2 + 1)],
            Some(funding(terms(None))),
            0,
        );
        assert!(rates(&published).is_empty(), "{published:?}");
    }

    #[test]
    fn a_river_without_funding_publishes_no_rates() {
        let ticks = vec![trade(100, 1), trade(200, INTERVAL + 1)];
        assert!(rates(&frames(ticks.clone(), None, 0)).is_empty());
        let mut zero = terms(None);
        zero.interval_ns = 0;
        assert!(
            rates(&frames(ticks, Some(funding(zero)), 0)).is_empty(),
            "a zero interval enumerates no instants"
        );
    }

    #[test]
    fn the_exhaustion_flush_publishes_instants_the_sweeper_may_already_have_charged() {
        // The cursor ends at the first trade; the boat's sim clock stands two
        // intervals later, where the sweeper has already charged both.
        let published = frames(
            vec![trade(100, 1)],
            Some(funding(terms(None))),
            INTERVAL * 2 + 1,
        );
        assert_eq!(
            rates(&published)
                .into_iter()
                .map(|(ts_event, _)| ts_event)
                .collect::<Vec<_>>(),
            vec![INTERVAL, INTERVAL * 2]
        );
    }

    #[test]
    fn the_exhaustion_flush_publishes_nothing_for_a_boat_that_never_traded() {
        let published = frames(vec![quote(1)], Some(funding(terms(None))), INTERVAL * 2 + 1);
        assert!(rates(&published).is_empty(), "{published:?}");
    }

    /// The index materialization gate, shown biting in both directions with the
    /// same terms and the same ticks. An unmaterialized index must leave the
    /// premium at zero rather than spend a river nobody asked for; a
    /// materialized one must actually move the rate.
    #[test]
    fn the_index_gate_bites_in_both_directions() {
        let terms = terms_over(INDEX_INTERVAL, Some(INDEX_SYMBOL));
        let ticks = || {
            vec![
                trade(100, INDEX_INTERVAL - 1),
                trade(100, INDEX_INTERVAL + 1),
            ]
        };
        let bare = crate::fills::test_rivers_with_a_second_symbol();
        assert!(
            !bare
                .materialized_symbols()
                .iter()
                .any(|symbol| symbol == INDEX_SYMBOL),
            "the index river must start unmaterialized or this gate proves nothing"
        );
        let unmaterialized = rates(&frames(
            ticks(),
            Some(TapeFunding {
                symbol: SYMBOL.into(),
                terms: terms.clone(),
                rivers: Arc::clone(&bare),
            }),
            0,
        ));
        assert_eq!(
            unmaterialized,
            vec![(INDEX_INTERVAL, terms.interest)],
            "an unmaterialized index leaves the bare interest"
        );
        assert!(
            !bare
                .materialized_symbols()
                .iter()
                .any(|symbol| symbol == INDEX_SYMBOL),
            "the refused read must not have spent the river it refused to read"
        );

        let rivers = materialized_index_rivers();
        let index = index_at(&rivers, INDEX_INTERVAL);
        let published = rates(&frames(
            ticks(),
            Some(TapeFunding {
                symbol: SYMBOL.into(),
                terms: terms.clone(),
                rivers,
            }),
            0,
        ));
        assert_eq!(
            published,
            vec![(INDEX_INTERVAL, terms.rate(Decimal::from(100), index))]
        );
        assert_ne!(
            published, unmaterialized,
            "a materialized index must move the rate, or the gate is vacuous"
        );
    }
}
