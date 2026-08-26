// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Plumbing shared by both [`super::data::MogwaiDataClient`] and
//! [`super::exec::MogwaiExecutionClient`]: the havoc dispatch pipeline
//! (`HavocFilter` and the latency pump), the lock/task-tracking
//! helpers, the instrument cache, the clock/url glue, and the small
//! config-to-havoc mapping functions both clients read from
//! `Option<HavocSpec>`.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, ensure};
use mogwai_protocol::{
    ConnHavoc, HavocLatency, HavocSpec, InboundHavoc, InstrumentDef, SimClock, Symbol, VenueClock,
    VenueMessage,
};
use nautilus_common::messages::DataEvent;
use nautilus_core::UnixNanos;
use nautilus_model::identifiers::InstrumentId;
use nautilus_network::http::HttpClient;
use rand::{RngExt, SeedableRng, rngs::StdRng};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use crate::{
    clock::fetch_clock,
    convert,
    lifecycle::{HttpQuota, IDENTITY_NOT_REPORTED, IDENTITY_UNREACHABLE, RunIdentityCheck},
};

/// One message queued for timed delivery through the latency pump: the wall
/// deadline it must not be released before, and the message to hand to the sink.
pub(crate) type HavocDelivery = (Instant, VenueMessage);

/// Wall deadline for a per-message havoc delay, anchored at NOW. Call it the
/// moment the message arrives (i.e. right after `filter.apply`); a zero delay
/// yields a deadline of now, so the pump releases the message immediately.
pub(crate) fn havoc_deadline(sim: SimClock, delay: Duration) -> Instant {
    Instant::now() + sim.wall_duration(duration_to_nanos(delay))
}

/// Sleep until a wall `deadline`, returning immediately if it has already
/// elapsed. Shared by the latency pump and the poll drain's anchored delivery.
async fn sleep_until_wall(deadline: Instant) {
    let now = Instant::now();
    if deadline > now {
        tokio::time::sleep(deadline - now).await;
    }
}

/// Apply the filter to one inbound `msg` and queue each resulting wire message
/// for timed delivery through [`spawn_latency_pump`], anchoring every message's
/// release deadline at its arrival. Unlike the inline-sleep `dispatch_havoc`,
/// this does NOT block on the delay - the enqueue is immediate, so the caller's
/// select loop stays responsive to pings/commands and a burst pipelines instead
/// of serializing (AD4). A dropped receiver (the pump gone) discards the
/// message, which only happens during teardown.
pub(crate) fn enqueue_havoc(
    filter: &mut HavocFilter,
    msg: VenueMessage,
    sim: SimClock,
    deliver_tx: &UnboundedSender<HavocDelivery>,
) {
    for (msg, delay) in filter.apply(msg) {
        drop(deliver_tx.send((havoc_deadline(sim, delay), msg)));
    }
}

/// The `flush()` twin of [`enqueue_havoc`] for disconnect teardown: queues any
/// reorder-held message through the same pump so it stays ordered behind the
/// events already enqueued from this connection.
pub(crate) fn flush_havoc_into_pump(
    filter: &mut HavocFilter,
    sim: SimClock,
    deliver_tx: &UnboundedSender<HavocDelivery>,
) {
    for (msg, delay) in filter.flush() {
        drop(deliver_tx.send((havoc_deadline(sim, delay), msg)));
    }
}

/// The inbound latency pump: drains queued `(deadline, message)` deliveries in
/// order, sleeping only until each message's own arrival-anchored deadline
/// before handing it to `sink`.
///
/// Because deadlines are anchored at arrival (not at the previous message's
/// release), a burst of same-delay messages collapses to a single delay window
/// instead of compounding: the first sleeps out the delay, and each subsequent
/// message finds its deadline already elapsed and forwards at once. This models
/// a network that delays every frame in parallel at full throughput, replacing
/// the old inline `sleep_havoc_delay` that realized the delay as inter-message
/// spacing - a ~33 msg/s ceiling that head-of-line-blocked pings/commands and
/// grew the inbound queue without bound under any burst (AD4). It mirrors the
/// deadline discipline of the mogwai venue's own `spawn_exec_pump`.
///
/// Ordering is preserved by construction (a single task over an ordered
/// channel, and arrival-anchored deadlines are monotone for equal delays). The
/// pump ends when every `deliver_tx` is dropped (its receiver closes); `stop()`
/// also aborts it via task tracking, discarding any still-in-flight delayed
/// messages exactly as the venue's pump does on disconnect.
pub(crate) fn spawn_latency_pump<F, Fut>(
    mut deliver_rx: UnboundedReceiver<HavocDelivery>,
    mut sink: F,
) -> JoinHandle<()>
where
    F: FnMut(VenueMessage) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send,
{
    tokio::spawn(async move {
        while let Some((deadline, msg)) = deliver_rx.recv().await {
            sleep_until_wall(deadline).await;
            sink(msg).await;
        }
    })
}

pub(crate) fn lock_recover<'a, T>(
    mutex: &'a Mutex<T>,
    label: &str,
) -> std::sync::MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(|poisoned| {
        tracing::warn!(lock = label, "recovering from poisoned mutex");
        poisoned.into_inner()
    })
}
/// Records a spawned task's handle so `stop()`/`disconnect()` can abort it
/// (AD17/AE19: a spawned `request_*` handler, instrument reseed or order-status
/// probe left running across a disconnect otherwise keeps issuing HTTP
/// requests, racing the HttpQuota, and sends into a possibly-dropped sink after
/// the client stopped). Already-finished
/// handles are pruned on every insert so the vec is bounded by the count of
/// in-flight tasks rather than growing once per short-lived request over the
/// client's lifetime. Shared behind the `Arc<Mutex<..>>` so the `&self` request
/// handlers can track their task alongside the `&mut self` connect path.
pub(crate) fn track_task(handles: &Arc<Mutex<Vec<JoinHandle<()>>>>, handle: JoinHandle<()>) {
    let mut handles = lock_recover(handles, "task handles");
    handles.retain(|h| !h.is_finished());
    handles.push(handle);
}
/// Aborts and clears every tracked task handle. Shared by both clients' `stop()`.
pub(crate) fn abort_tasks(handles: &Arc<Mutex<Vec<JoinHandle<()>>>>) {
    for handle in lock_recover(handles, "task handles").drain(..) {
        handle.abort();
    }
}
pub(crate) fn instrument_def(
    instruments: &Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    symbol: &str,
) -> Option<InstrumentDef> {
    lock_recover(instruments, "instrument").get(symbol).cloned()
}

/// Warns exactly once per symbol that a streamed frame arrived with no seeded
/// `InstrumentDef`, so the symbol's data is being black-holed. The instruments
/// map is seeded once at connect; a symbol subscribed but absent from that seed
/// (a venue config change or a later-added instrument) otherwise streams into
/// nothing with zero diagnostics. The drain (`emit_trade`, the quote arm) has
/// no HTTP handle to re-seed, so it can only surface the miss - the spawned
/// request handlers, which do have async and HTTP context, self-heal via
/// `ensure_instrument`. The
/// dedup keeps a per-trade warn from flooding the log for a genuinely-missing
/// symbol. The set belongs to one data client, so another venue client still
/// reports its own first miss.
pub(crate) fn warn_missing_instrument_once(
    warned: &Arc<Mutex<std::collections::HashSet<String>>>,
    symbol: &str,
) {
    let mut warned = lock_recover(warned, "missing-instrument warn set");
    // The set is keyed by an arbitrary wire symbol, so it is bounded for the
    // same reason the orphan quote cache is: a venue emitting endless distinct
    // unknown symbols must not grow adapter state without limit. Past the
    // ceiling the dedup stops recording and every further miss warns, which is
    // the loud direction to fail in for a diagnostic.
    const MAX_WARNED_SYMBOLS: usize = 256;
    if warned.len() >= MAX_WARNED_SYMBOLS && !warned.contains(symbol) {
        tracing::warn!(
            %symbol,
            "no instrument def for a streamed symbol, and the warn dedup set is full"
        );
        return;
    }
    if warned.insert(symbol.to_string()) {
        tracing::warn!(
            %symbol,
            "no instrument def for a streamed symbol; its data is black-holed \
             until the instrument is seeded (venue config change or later-added \
             instrument)"
        );
    }
}

pub(crate) async fn seed_instruments(
    http: &HttpClient,
    quota: &HttpQuota,
    base: &str,
    instruments: &Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
) -> anyhow::Result<()> {
    let defs = fetch_instruments(http, quota, base).await?;
    cache_instruments(instruments, defs);
    Ok(())
}

/// Push every seeded instrument into the Nautilus cache on connect.
///
/// Seeding only fills the adapter's local `InstrumentDef` map, which the
/// adapter consults for price/precision conversion. Nautilus's own cache is a
/// separate store fed exclusively by `DataEvent::Instrument`, and a host's
/// executor refuses to process a bar whose instrument is absent from that cache
/// (a desync guard against advancing the shadow with no real order). A forward
/// run that subscribes to bars but never to the instrument itself would
/// therefore have the cache stay empty and every bar refused. Emitting the
/// seeded defs here populates the cache the instant the data client connects,
/// independent of whatever the strategy later subscribes to.
pub(crate) fn emit_seeded_instruments(
    sink: &UnboundedSender<DataEvent>,
    instruments: &Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    sim: SimClock,
) {
    let ts_init = now_unix_nanos(sim);
    let defs: Vec<InstrumentDef> = lock_recover(instruments, "instrument")
        .values()
        .cloned()
        .collect();
    for def in defs {
        if let Some(instrument) = instrument_any_or_warn(&def, ts_init) {
            drop(sink.send(DataEvent::Instrument(instrument)));
        }
    }
}

/// Converts a seeded/fetched `InstrumentDef` into a nautilus instrument,
/// warning loudly on failure instead of swallowing it. `convert::instrument_any`
/// errors when the def carries a base/quote currency unknown to nautilus (an
/// exotic pair), and a silent drop here cascades: the instrument never reaches
/// the Nautilus cache, so a host executor gating on it refuses every single bar for the
/// symbol with no log line pointing at the cause - the exact failure
/// `emit_seeded_instruments` exists to prevent. Naming the symbol and the error
/// at every swallow site turns that into a diagnosable log.
pub(crate) fn instrument_any_or_warn(
    def: &InstrumentDef,
    ts_init: UnixNanos,
) -> Option<nautilus_model::instruments::InstrumentAny> {
    convert::instrument_any(def, ts_init)
        .map_err(|err| {
            tracing::warn!(
                symbol = %def.symbol,
                error = %err,
                "dropping instrument: nautilus has no faithful representation for it; \
                 downstream consumers will refuse every bar for this symbol"
            );
        })
        .ok()
}

pub(crate) async fn ensure_instrument(
    http: &HttpClient,
    quota: &HttpQuota,
    base: &str,
    instruments: &Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    symbol: &str,
) -> anyhow::Result<InstrumentDef> {
    if let Some(def) = instrument_def(instruments, symbol) {
        return Ok(def);
    }
    seed_instruments(http, quota, base, instruments).await?;
    instrument_def(instruments, symbol).with_context(|| format!("unknown instrument {symbol}"))
}

pub(crate) fn cache_instruments(
    instruments: &Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    defs: Vec<InstrumentDef>,
) {
    let mut cache = lock_recover(instruments, "instrument");
    for def in defs {
        cache.insert(std::sync::Arc::clone(&def.symbol), def);
    }
}

pub(crate) async fn fetch_instruments(
    http: &HttpClient,
    quota: &HttpQuota,
    base: &str,
) -> anyhow::Result<Vec<InstrumentDef>> {
    quota.wait().await;
    let response = http
        .get(
            join_url(
                base,
                mogwai_protocol::routes::segment(mogwai_protocol::routes::INSTRUMENTS),
            ),
            None,
            None,
            Some(mogwai_protocol::DEFAULT_REQUEST_TIMEOUT_SECS),
            None,
        )
        .await
        .context("fetch instruments")?;
    ensure!(
        response.status.is_success(),
        "fetch instruments returned {}",
        response.status.as_u16()
    );
    serde_json::from_slice(&response.body).context("decode instruments")
}
pub(crate) struct HavocFilter {
    latency: Option<HavocLatency>,
    drop_prob: f64,
    duplicate_prob: f64,
    reorder_prob: f64,
    rng: StdRng,
    held: Option<VenueMessage>,
    held_generation: u64,
}

impl HavocFilter {
    pub(crate) fn from_inbound(inbound: &InboundHavoc) -> Self {
        Self {
            latency: inbound.latency,
            drop_prob: inbound.drop_prob,
            duplicate_prob: inbound.duplicate_prob,
            reorder_prob: inbound.reorder_prob,
            rng: inbound
                .seed
                .map_or_else(|| StdRng::from_rng(&mut rand::rng()), StdRng::seed_from_u64),
            held: None,
            held_generation: 0,
        }
    }

    fn apply(&mut self, msg: VenueMessage) -> Vec<(VenueMessage, Duration)> {
        let mut candidates = Vec::new();
        if let Some(held) = self.held.take() {
            candidates.push(msg);
            candidates.push(held);
        } else if self.draw(self.reorder_prob) {
            self.held_generation = self.held_generation.wrapping_add(1);
            self.held = Some(msg);
            return Vec::new();
        } else {
            candidates.push(msg);
        }
        self.emit_candidates(candidates)
    }

    fn flush(&mut self) -> Vec<(VenueMessage, Duration)> {
        let Some(held) = self.held.take() else {
            return Vec::new();
        };
        self.emit_candidates(vec![held])
    }

    pub(crate) fn held_token(&self) -> Option<u64> {
        self.held.as_ref().map(|_| self.held_generation)
    }

    fn flush_if_token(&mut self, token: u64) -> Vec<(VenueMessage, Duration)> {
        if self.held_token() == Some(token) {
            self.flush()
        } else {
            Vec::new()
        }
    }

    fn emit_candidates(&mut self, candidates: Vec<VenueMessage>) -> Vec<(VenueMessage, Duration)> {
        let mut out = Vec::new();
        for msg in candidates {
            if self.draw(self.drop_prob) {
                continue;
            }
            let delay = self.delay_for(&msg);
            if self.draw(self.duplicate_prob) {
                out.push((msg.clone(), delay));
            }
            out.push((msg, delay));
        }
        out
    }

    pub(crate) fn delay_for(&self, msg: &VenueMessage) -> Duration {
        let category = msg.category();
        let baseline = mogwai_protocol::BASELINE_LATENCY.delay_for(category);
        let armed = self
            .latency
            .map_or(Duration::ZERO, |latency| latency.delay_for(category));
        baseline + armed
    }

    pub(crate) fn draw(&mut self, probability: f64) -> bool {
        probability > 0.0 && self.rng.random::<f64>() < probability
    }
}

/// Gives a reorder hold a deadline no later than one wall second. A following message
/// still releases it immediately; the token prevents an older timer from
/// flushing a later hold on the same connection.
pub(crate) fn schedule_reorder_flush(
    filter: Arc<tokio::sync::Mutex<HavocFilter>>,
    token: u64,
    sim: SimClock,
    deliver_tx: UnboundedSender<HavocDelivery>,
) {
    tokio::spawn(async move {
        tokio::time::sleep(sim.wall_duration(1_000_000_000).min(Duration::from_secs(1))).await;
        let mut filter = filter.lock().await;
        for (msg, delay) in filter.flush_if_token(token) {
            drop(deliver_tx.send((havoc_deadline(sim, delay), msg)));
        }
    });
}
pub(crate) fn inbound_havoc(spec: &Option<HavocSpec>) -> InboundHavoc {
    spec.as_ref()
        .map_or_else(InboundHavoc::default, |spec| spec.inbound.clone())
}
pub(crate) fn conn_havoc(spec: &Option<HavocSpec>) -> ConnHavoc {
    spec.as_ref()
        .map_or_else(ConnHavoc::default, |spec| spec.conn)
}
/// Wall floor for the scaled HTTP request timeout, in seconds. Unlike the other
/// scaled durations (whose floor is the ~1ms tokio granularity), this one guards
/// a real local-IO round trip whose wall cost never compresses with `speed`:
/// dividing a sim-seconds timeout by a high `speed` would otherwise yield a
/// sub-second wall budget that the actual HTTP round trip blows, spuriously
/// timing out every order. Clamping up to one wall second keeps the request
/// survivable, and it imposes no ceiling on usable speed: the budget only ever
/// stops shrinking. What it costs instead is the configured number's meaning -
/// past `speed == configured`, the floor is what applies and the effective
/// budget is `speed` simulated seconds rather than the span the consumer asked
/// for. Deliberate, and reasoned out in `reference/clock.md`.
const MIN_WALL_REQUEST_TIMEOUT_SECS: u64 = 1;

pub(crate) fn request_timeout_secs(spec: &Option<HavocSpec>, sim: SimClock) -> u64 {
    let configured = conn_havoc(spec).request_timeout_secs;
    let sim_secs = if configured == 0 {
        mogwai_protocol::DEFAULT_REQUEST_TIMEOUT_SECS
    } else {
        configured
    };
    let wall_ns = sim.wall_span(sim_secs.saturating_mul(crate::clock::NANOS_PER_SEC));
    wall_ns
        .div_ceil(crate::clock::NANOS_PER_SEC)
        .max(MIN_WALL_REQUEST_TIMEOUT_SECS)
}
/// Wall backoff between clock-fetch retries. Small and fixed: this runs inline
/// in connect(), so it must not stall boot for long, but a couple of retries
/// ride out a transient blip before the identity fallback commits the whole
/// connection to the wrong time axis.
const CLOCK_FETCH_RETRY_DELAY: Duration = Duration::from_millis(200);
const CLOCK_FETCH_MAX_ATTEMPTS: u32 = 3;

pub(crate) async fn fetch_clock_or_identity(
    http: &HttpClient,
    http_base: &str,
) -> (VenueClock, bool) {
    // Retry before committing to identity (AD16): the identity fallback silently
    // puts every ts_init, havoc sleep, quota interval, backoff and timeout on the
    // wrong axis for the life of the connection if the venue actually runs at
    // speed != 1, and nothing re-fetches the clock later. A couple of quick
    // retries ride out a transient fetch blip; only a persistent failure falls
    // back, and then loudly.
    let mut last_err = None;
    for attempt in 0..CLOCK_FETCH_MAX_ATTEMPTS {
        match fetch_clock(http, http_base).await {
            Ok(clock) => {
                if attempt > 0 {
                    tracing::info!(attempt, "clock fetch recovered after retry");
                }
                return (clock, true);
            }
            Err(err) => {
                tracing::warn!(
                    %err,
                    attempt,
                    "clock fetch failed; retrying before the identity fallback"
                );
                last_err = Some(err);
                if attempt + 1 < CLOCK_FETCH_MAX_ATTEMPTS {
                    tokio::time::sleep(CLOCK_FETCH_RETRY_DELAY).await;
                }
            }
        }
    }
    tracing::error!(
        err = ?last_err,
        attempts = CLOCK_FETCH_MAX_ATTEMPTS,
        "clock fetch failed after retries; falling back to the identity mogwai \
         clock - if the venue runs at speed != 1, every ts_init, havoc sleep, \
         quota interval, backoff and timeout will be scaled on the wrong axis for \
         the life of this connection"
    );
    // The boolean preserves whether the floor is known. Zero is a valid floor
    // and must never double as the fallback sentinel.
    (
        VenueClock {
            sim: SimClock::identity(),
            venue_now_ns: 0,
            data_origin_ns: 0,
            warmup_ns: 0,
            // A synthesized fallback is not a boat's answer either.
        },
        false,
    )
}
/// Builds the per-dial run-identity check both clients hand to their connection
/// loop, or `None` when the config named no run to be bound to.
///
/// The probe is `GET /health`, which reports the run's seed - a value already
/// unique per run and already carried in the readiness record, so a launcher
/// that has one can bind its clients to it without any new plumbing.
///
/// Reachability and identity are separated deliberately. A health probe fails
/// for the same transport reasons a socket does, so an unanswered probe is
/// reported with the [`IDENTITY_UNREACHABLE`] prefix and never refuses the
/// connection; only a venue that answers, with a different run, does.
pub(crate) fn run_identity_check(
    http: HttpClient,
    quota: HttpQuota,
    http_base: String,
    expected: Option<u64>,
    label: &'static str,
) -> Option<RunIdentityCheck> {
    let Some(expected) = expected else {
        // Dialling blind, said once per client rather than per dial. An address
        // and the run it belongs to arrive together in the readiness record, so
        // a config without a seed is one that discarded the identity, never one
        // that could not have it - which is why this is worth a line rather
        // than being the silent default it looks like at the call site.
        //
        // What it costs: a venue frees its port before it exits and binds an
        // ephemeral one, so an address outlives the run that owned it. A client
        // that dials a recycled port reaches whoever holds it now - most likely
        // another mogwai venue, which speaks the wire perfectly and will serve
        // a tape from a different run without either side noticing.
        tracing::warn!(
            socket = label,
            base_url = %http_base,
            "no expected_run_seed: this client cannot tell its venue from another \
             holding the same address. Build the config with `for_run` from the \
             readiness record, or pass the record's `run_seed` alongside the address"
        );
        return None;
    };
    Some(Arc::new(move || {
        let http = http.clone();
        let quota = quota.clone();
        let url = join_url(
            &http_base,
            mogwai_protocol::routes::segment(mogwai_protocol::routes::HEALTH),
        );
        let probe: std::pin::Pin<Box<dyn Future<Output = Result<(), String>> + Send>> =
            Box::pin(async move {
                quota.wait().await;
                let response = http
                    .get(
                        url.clone(),
                        None,
                        None,
                        Some(mogwai_protocol::DEFAULT_REQUEST_TIMEOUT_SECS),
                        None,
                    )
                    .await
                    .map_err(|err| format!("{IDENTITY_UNREACHABLE}{url}: {err}"))?;
                if !response.status.is_success() {
                    return Err(format!(
                        "{IDENTITY_UNREACHABLE}{url} answered {}",
                        response.status.as_u16()
                    ));
                }
                let health: serde_json::Value = serde_json::from_slice(&response.body)
                    .map_err(|err| format!("{IDENTITY_UNREACHABLE}{url} is not JSON: {err}"))?;
                // A venue too old to report its run is unidentifiable rather than
                // wrong, and refusing it would make this field's arrival a breaking
                // change for a client that opted in.
                let Some(reported) = health.get("run_seed").and_then(serde_json::Value::as_u64)
                else {
                    return Err(format!(
                        "{IDENTITY_NOT_REPORTED}{url} answered without a run_seed; the venue \
                         predates run identity"
                    ));
                };
                if reported == expected {
                    Ok(())
                } else {
                    Err(format!(
                        "expected run {expected}, but this address is serving run {reported}"
                    ))
                }
            });
        probe
    }))
}

pub(crate) fn duration_to_nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}
pub(crate) fn join_url(base: &str, path: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), path)
}
pub(crate) fn symbol_from_instrument(instrument_id: InstrumentId) -> mogwai_protocol::Symbol {
    instrument_id.symbol.as_str().into()
}
/// Maps an optional request bound onto the u64 nanosecond axis, saturating
/// out-of-range datetimes at the axis bounds instead of dropping them to
/// `None`. `None` means "unbounded" to every caller, so silently mapping a
/// pre-epoch end to `None` would widen the request (an end of 1950 becoming
/// "until forever") and a far-future start would become "from origin"; clamping
/// preserves the requester's intent (a pre-epoch bound is the epoch, a
/// far-future bound is the axis ceiling) and lets a nonsense range come back
/// loudly empty rather than quietly full.
pub(crate) fn date_to_unix_nanos(date: Option<jiff::Timestamp>) -> Option<UnixNanos> {
    date.map(|ts| {
        // jiff spans years -9999..=9999 in i128 nanoseconds, far wider on both
        // sides than the u64 axis (which ends in 2554), so pick the side by the
        // sign and clamp to the near or far bound accordingly.
        let ns = ts.as_nanosecond();
        UnixNanos::from(u64::try_from(ns).unwrap_or(if ns < 0 { 0 } else { u64::MAX }))
    })
}
/// Refuse an off-river history request before spending a round trip on it. A
/// `start` below the published `data_origin` can never be served (the river
/// begins at the origin), so the round trip can only end in a venue `400` - or,
/// against a venue that does not refuse, an empty `200` the history request
/// cannot tell from "no trades happened". Failing here, naming both the requested start and the
/// floor, turns that into a loud, surfaced error at the request boundary
/// instead of a silent doomed fetch.
///
/// `data_origin == 0` is the "floor unknown" sentinel (the `/clock` fetch failed
/// and the client fell back to identity): the check is skipped so the venue's
/// own refusal stays authoritative. A `None` start means "from origin" and is
/// always on-river.
pub(crate) fn ensure_on_river(
    start: Option<UnixNanos>,
    data_origin: Option<u64>,
) -> anyhow::Result<()> {
    if let (Some(start), Some(data_origin)) = (start, data_origin)
        && start.as_u64() < data_origin
    {
        anyhow::bail!(
            "requested start {} precedes data_origin_ns {}; the mogwai river cannot serve before its origin",
            start.as_u64(),
            data_origin
        );
    }
    Ok(())
}
pub(crate) fn now_unix_nanos(sim: SimClock) -> UnixNanos {
    // Thin typed wrapper over the shared wall read plus the fetched simulated
    // clock. The underlying reader keeps the saturating contract; the affine
    // map then places adapter-side `ts_init` on the same axis as the venue.
    UnixNanos::from(sim.sim_ns(mogwai_protocol::now_unix_nanos()))
}
/// Wait for the reader task to report its socket open, under the same deadline
/// that bounds the dial itself.
///
/// The bound is supplied rather than chosen here, and that is the point. It was
/// five hardcoded seconds, from when the first boarding always found its water
/// already warm. Nothing is warm at boot any more: every river is synthesized
/// inside the request that first names it, so an upgrade legitimately takes as
/// long as a cold river's whole warmup span. A dial that the consumer may bound
/// at sixty seconds, sitting behind an outer wait that gives up at five, fails
/// the connect while the venue is still doing exactly what it was asked to do -
/// and no amount of retrying repairs it, because the retry meets the same wall.
///
/// Wall time, never sim-scaled: synthesis and the websocket upgrade are host
/// work, and an accelerated clock does not make them finish sooner.
/// How long `wait_connected` will sit on the connection notification before
/// re-reading the flag anyway. See the loop for why a notifier still wants a
/// backstop; it is deliberately far coarser than the 10 ms poll it replaced.
const NOTIFY_BACKSTOP: Duration = Duration::from_millis(250);

pub(crate) async fn wait_connected(
    connected: &Arc<AtomicBool>,
    notify: &Arc<tokio::sync::Notify>,
    ws_url: &str,
    timeout: Duration,
) -> anyhow::Result<()> {
    let wait = async {
        loop {
            // Register before checking the flag, so a notification published
            // between the load and the await cannot be lost. `enable` is what
            // makes the registration happen here rather than at first poll.
            let notified = notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if connected.load(Ordering::Relaxed) {
                return;
            }
            // Bounded, and the bound is a backstop rather than a poll interval.
            // The notification is what wakes this on the normal path - that is
            // the whole reason it replaced a 10 ms sleep loop, which spent
            // roughly five hundred wakeups on a slow boot. What the bound buys
            // is that the flag has exactly one publisher today
            // (`run_ws_connection_inner`, which stores and notifies in adjacent
            // statements) and nothing makes it stay that way: a future store
            // that forgets to notify wedges this until `timeout` expires and
            // fails a connect that had already succeeded. That is not
            // hypothetical - deleting the `notify_waiters` call is how this
            // wait was bite-checked, and every socket test hung rather than
            // failing on anything that named the cause. Re-reading the flag a
            // few times a second costs nothing measurable and turns that class
            // of mistake into a late connect instead of a dead client.
            drop(tokio::time::timeout(NOTIFY_BACKSTOP, notified).await);
        }
    };
    if tokio::time::timeout(timeout, wait).await.is_ok() {
        return Ok(());
    }
    anyhow::bail!(
        "connect websocket {ws_url} timed out after {secs}s; a venue synthesizing a cold river \
         inside this upgrade can legitimately take longer, and `dial_timeout_secs` raises this",
        secs = timeout.as_secs()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ConnHavoc.request_timeout_secs == 0` is documented as "keeps 30s", and
    /// this is the only thing that holds it. `DEFAULT_REQUEST_TIMEOUT_SECS` used
    /// to be pinned in `mogwai-protocol` by an assertion against its own
    /// literal, which has no second referent: the constant is defined once and
    /// every adapter site reads it, so that assertion could only restate the
    /// definition. The claim with a referent is the substitution itself - that an
    /// unconfigured client gets the shipped default rather than a zero-second
    /// timeout that fails every request instantly. The clamp is deliberately
    /// out of the way (speed 1.0, so no scaling and no `MIN_WALL` floor).
    #[test]
    fn an_unset_request_timeout_takes_the_shipped_default() {
        let unscaled = SimClock::identity();
        assert_eq!(
            request_timeout_secs(&None, unscaled),
            mogwai_protocol::DEFAULT_REQUEST_TIMEOUT_SECS
        );
        // `ConnHavoc::default()` already carries `request_timeout_secs: 0`, so
        // this case states no timeout of its own: what it varies is the spec itself
        // being present at all, taking `conn_havoc`'s `Some` branch rather than
        // the `None` one above, and it must land on the same default.
        assert_eq!(
            request_timeout_secs(&Some(HavocSpec::default()), unscaled),
            mogwai_protocol::DEFAULT_REQUEST_TIMEOUT_SECS,
            "an unset timeout inside a spec is the same 'no opinion' as no spec"
        );
        let stated = HavocSpec {
            conn: ConnHavoc {
                request_timeout_secs: 7,
                ..ConnHavoc::default()
            },
            ..HavocSpec::default()
        };
        assert_eq!(
            request_timeout_secs(&Some(stated), unscaled),
            7,
            "a stated timeout is not overwritten by the default"
        );
    }

    #[test]
    fn an_unknown_floor_skips_the_guard_but_a_zero_floor_enforces_it() {
        ensure_on_river(Some(UnixNanos::from(5)), None).expect("unknown floor skips guard");
        ensure_on_river(Some(UnixNanos::from(5)), Some(0)).expect("zero floor accepts five");
        let err = ensure_on_river(Some(UnixNanos::from(5)), Some(10))
            .expect_err("known floor rejects an earlier start");
        assert!(err.to_string().contains("data_origin_ns 10"), "{err}");
    }

    #[test]
    fn date_to_unix_nanos_maps_in_range_datetimes_exactly() {
        let dt = jiff::Timestamp::from_nanosecond(1_234_567_890_123_456_789).expect("in range");
        assert_eq!(
            date_to_unix_nanos(Some(dt)),
            Some(UnixNanos::from(1_234_567_890_123_456_789u64))
        );
        assert_eq!(date_to_unix_nanos(None), None);
    }

    #[test]
    fn date_to_unix_nanos_saturates_out_of_range_instead_of_unbounding() {
        // Pre-epoch: clamps to the epoch, so an end of 1950 stays an empty
        // range rather than becoming "forever".
        let pre_epoch: jiff::Timestamp = "1950-01-01T00:00:00Z".parse().expect("parses");
        assert_eq!(
            date_to_unix_nanos(Some(pre_epoch)),
            Some(UnixNanos::from(0u64))
        );

        // Far pre-epoch, well outside the axis entirely: same clamp to epoch.
        let ancient: jiff::Timestamp = "1500-01-01T00:00:00Z".parse().expect("parses");
        assert_eq!(
            date_to_unix_nanos(Some(ancient)),
            Some(UnixNanos::from(0u64))
        );

        // Past 2554, where the u64 nanosecond axis ends: clamps to the ceiling,
        // so a far-future start stays a loud empty range rather than becoming
        // "from origin".
        let far_future: jiff::Timestamp = "3000-01-01T00:00:00Z".parse().expect("parses");
        assert_eq!(
            date_to_unix_nanos(Some(far_future)),
            Some(UnixNanos::from(u64::MAX))
        );
    }

    /// A config with no expected seed produces NO probe, which is the blind
    /// path the warning beside it names. Pinned because the absence is the
    /// whole behaviour: `run_ws_connection` treats `None` as "nothing to
    /// check", so a regression that started returning a probe here would make
    /// every seedless client refuse its own venue, and one that stopped
    /// returning a probe when a seed IS set would silently disable the check.
    #[test]
    fn an_absent_expected_seed_installs_no_identity_probe() {
        let http = HttpClient::new(
            std::collections::HashMap::new(),
            vec![],
            vec![],
            None,
            Some(mogwai_protocol::DEFAULT_REQUEST_TIMEOUT_SECS),
            None,
        )
        .expect("http client builds");
        let quota = HttpQuota::from_conn(&ConnHavoc::default(), SimClock::identity());
        assert!(
            run_identity_check(
                http.clone(),
                quota.clone(),
                "http://127.0.0.1:1".to_owned(),
                None,
                "data",
            )
            .is_none(),
            "no expected seed means no probe to run"
        );
        assert!(
            run_identity_check(
                http,
                quota,
                "http://127.0.0.1:1".to_owned(),
                Some(7),
                "data",
            )
            .is_some(),
            "an expected seed installs the probe"
        );
    }

    #[tokio::test]
    async fn latency_pump_pipelines_a_burst_instead_of_serializing() {
        // AD4: messages that arrive together must drain in ~one delay window, not
        // one-delay-per-message. The old inline sleep serialized the 30 ms
        // baseline latency into inter-message spacing (~33 msg/s); the pump
        // anchors each deadline at arrival, so a simultaneous burst collapses to
        // a single window while preserving order.
        let sim = SimClock::identity();
        let mut filter = HavocFilter::from_inbound(&InboundHavoc::default());
        let per_msg = filter.delay_for(&VenueMessage::Heartbeat { ts_event: 0 });
        assert!(
            !per_msg.is_zero(),
            "baseline latency must be nonzero for this test"
        );

        let (deliver_tx, deliver_rx) = tokio::sync::mpsc::unbounded_channel::<HavocDelivery>();
        let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<VenueMessage>();
        let pump = spawn_latency_pump(deliver_rx, move |msg| {
            let out_tx = out_tx.clone();
            async move {
                drop(out_tx.send(msg));
            }
        });

        const N: u64 = 40;
        let start = Instant::now();
        for i in 0..N {
            enqueue_havoc(
                &mut filter,
                VenueMessage::Heartbeat { ts_event: i },
                sim,
                &deliver_tx,
            );
        }
        drop(deliver_tx);

        for expected in 0..N {
            let got = out_rx.recv().await.expect("every message is delivered");
            let VenueMessage::Heartbeat { ts_event } = got else {
                panic!("unexpected message on the pump output");
            };
            assert_eq!(ts_event, expected, "the pump preserves arrival order");
        }
        let elapsed = start.elapsed();
        // The bound is stated against the defect, never against the measurement.
        // A pipelined drain is one window (measured 31.1-31.6 ms over 60 runs
        // against a 30 ms `per_msg`); the serialization this exists to catch is
        // forty of them, 1.2 s. Anything strictly between is not a shape the
        // pump can take - the deadlines are either arrival-anchored or they
        // compound - so a quarter of the serial cost separates the two as
        // cleanly as three windows did while sitting an order of magnitude
        // further from the pipelined side, which is the side a loaded host
        // pushes on.
        //
        // And it catches both shapes, which was doubted and then measured. The
        // worry was that a pump re-anchoring each deadline at the previous
        // release rather than at arrival would slip through, on the reasoning
        // that a simultaneous burst makes the two anchors coincide. They
        // coincide for the first message only: chaining releases gives
        // `i * per_msg`, so this burst finishes at 40 windows, and the defect
        // installed as a text edit in `havoc_deadline` fails this assertion at
        // 1.202 s against its 300 ms bound. A staggered-arrival twin was built
        // to cover the supposed gap, measured 127-129 ms honest against the same
        // 600 ms compounded defect, and deleted as strictly weaker than this
        // one - twenty windows of separation where this has forty. Per-message
        // spacing and compounding deadlines are the same failure here because
        // the only way to space the output is to stop anchoring at arrival.
        let serial = per_msg * u32::try_from(N).unwrap();
        assert!(
            elapsed < serial / 4,
            "the burst drained in {elapsed:?}; one pipelined window is ~{per_msg:?} \
             and a serial drain would need ~{serial:?}"
        );
        drop(pump);
    }

    #[test]
    fn a_reorder_hold_is_released_only_by_its_own_deadline_token() {
        let mut filter = HavocFilter::from_inbound(&InboundHavoc {
            reorder_prob: 1.0,
            seed: Some(7),
            ..InboundHavoc::default()
        });
        drop(filter.apply(VenueMessage::RunComplete {
            sim_now_ns: 1,
            elapsed_ns: 2,
        }));
        let token = filter.held_token().expect("the message is held");
        assert!(
            filter.flush_if_token(token.wrapping_add(1)).is_empty(),
            "a timer belonging to a different hold released this one"
        );
        assert_eq!(
            filter.held_token(),
            Some(token),
            "the wrong-token attempt must leave the hold in place"
        );
        let released = filter.flush_if_token(token);
        assert!(
            matches!(
                released.as_slice(),
                [(
                    VenueMessage::RunComplete {
                        sim_now_ns: 1,
                        elapsed_ns: 2
                    },
                    _
                )]
            ),
            "the hold's own token must release exactly the held message, got {released:?}"
        );
        assert_eq!(
            filter.held_token(),
            None,
            "a released hold must not stay claimable by a second timer"
        );
    }

    /// The deadline half of the same fix, driven through the real spawn.
    ///
    /// `flush_if_token` alone cannot show that a hold is ever released without
    /// a following message - which was the whole of the finding, since the
    /// execution socket's arrivals are order-driven and a quiet strategy
    /// produces none. This drives `schedule_reorder_flush` and waits on the
    /// delivery channel, so what is pinned is that the hold has a bound at all.
    /// A generous wait, because what is under test is the existence of the
    /// deadline and not its precision.
    #[tokio::test]
    async fn a_reorder_hold_is_released_by_its_deadline_with_no_following_message() {
        let filter = Arc::new(tokio::sync::Mutex::new(HavocFilter::from_inbound(
            &InboundHavoc {
                reorder_prob: 1.0,
                seed: Some(7),
                ..InboundHavoc::default()
            },
        )));
        let (deliver_tx, mut deliver_rx) = tokio::sync::mpsc::unbounded_channel();
        let sim = SimClock::identity();
        {
            let mut guard = filter.lock().await;
            enqueue_havoc(
                &mut guard,
                VenueMessage::RunComplete {
                    sim_now_ns: 1,
                    elapsed_ns: 2,
                },
                sim,
                &deliver_tx,
            );
            let token = guard.held_token().expect("the reorder draw held it");
            schedule_reorder_flush(Arc::clone(&filter), token, sim, deliver_tx.clone());
        }
        assert!(
            deliver_rx.try_recv().is_err(),
            "the hold must not be delivered before its deadline"
        );
        let delivered = tokio::time::timeout(Duration::from_secs(5), deliver_rx.recv())
            .await
            .expect("the hold's deadline must release it with no following message");
        assert!(
            matches!(
                delivered,
                Some((
                    _,
                    VenueMessage::RunComplete {
                        sim_now_ns: 1,
                        elapsed_ns: 2
                    }
                ))
            ),
            "the deadline released something other than the held message: {delivered:?}"
        );
        assert_eq!(
            filter.lock().await.held_token(),
            None,
            "the deadline must clear the hold it released"
        );
    }
}
