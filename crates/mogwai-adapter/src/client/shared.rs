// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Plumbing shared by both [`super::data::MogwaiDataClient`] and
//! [`super::exec::MogwaiExecutionClient`]: the havoc dispatch pipeline
//! (`HavocFilter`, `dispatch_havoc`/`flush_havoc`), the lock/task-tracking
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
    ClientHavoc, ConnHavoc, HavocLatency, HavocSpec, InstrumentDef, MarketRegime, ServerClock,
    ServerMessage, SimClock, Symbol,
};
use nautilus_common::messages::DataEvent;
use nautilus_core::UnixNanos;
use nautilus_model::identifiers::InstrumentId;
use nautilus_network::http::HttpClient;
use rand::{RngExt, SeedableRng, rngs::StdRng};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use crate::{clock::fetch_clock, convert, lifecycle::HttpQuota};

/// One message queued for timed delivery through the latency pump: the wall
/// deadline it must not be released before, and the message to hand to the sink.
pub(crate) type HavocDelivery = (Instant, ServerMessage);

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

/// Drain one inbound `msg`'s filtered expansion inline, sleeping only until each
/// message's deadline relative to a shared `arrival` anchor before handing it to
/// `handle`. Anchoring every message of a batch at one arrival instant is what
/// stops the per-message delays from compounding: a fetched page of trades all
/// anchor at the fetch instant, so the first sleeps out the delay and the rest
/// find their deadline already elapsed and forward at once - one delay window
/// for the page instead of `page_len * delay` (AD4). Used by the poll transport,
/// which drains a fetched page sequentially on its own task (no select loop or
/// ping to keep responsive, and its `sim` can change mid-run via the AD6
/// self-heal), so it keeps the inline shape and reads the live `sim` here rather
/// than handing delivery to a separate pump that captured `sim` at spawn.
pub(crate) async fn drain_havoc_anchored<F, Fut>(
    filter: &mut HavocFilter,
    msg: ServerMessage,
    sim: SimClock,
    arrival: Instant,
    mut handle: F,
) where
    F: FnMut(ServerMessage) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    for (msg, delay) in filter.apply(msg) {
        sleep_until_wall(arrival + sim.wall_duration(duration_to_nanos(delay))).await;
        handle(msg).await;
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
    msg: ServerMessage,
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
/// SPACING - a ~33 msg/s ceiling that head-of-line-blocked pings/commands and
/// grew the inbound queue without bound under any burst (AD4). It mirrors the
/// deadline discipline of the mogwai server's own `spawn_exec_pump`.
///
/// Ordering is preserved by construction (a single task over an ordered
/// channel, and arrival-anchored deadlines are monotone for equal delays). The
/// pump ends when every `deliver_tx` is dropped (its receiver closes); `stop()`
/// also aborts it via task tracking, discarding any still-in-flight delayed
/// messages exactly as the server's pump does on disconnect.
pub(crate) fn spawn_latency_pump<F, Fut>(
    mut deliver_rx: UnboundedReceiver<HavocDelivery>,
    mut sink: F,
) -> JoinHandle<()>
where
    F: FnMut(ServerMessage) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send,
{
    tokio::spawn(async move {
        while let Some((deadline, msg)) = deliver_rx.recv().await {
            sleep_until_wall(deadline).await;
            sink(msg).await;
        }
    })
}

/// Drains the havoc-mangled expansion of one inbound `msg` and routes each
/// resulting wire message through `handle`, sleeping the per-message delay
/// first. Retained for the short-lived HTTP order-dispatch response drain
/// (a handful of causally-ordered events on a dedicated task, with no select
/// loop or ping to head-of-line-block); the live streaming drains use the
/// pipelined [`spawn_latency_pump`] instead (AD4). Generic over the per-message
/// sink so the market path (which forwards to `handle_market_message`, async)
/// and the exec path (which forwards to `handle_exec_message`, wrapped in an
/// async block) share one control flow. `flush_havoc` is the same loop over
/// `filter.flush()` for the disconnect teardown that emits any
/// divergence-held events.
pub(crate) async fn dispatch_havoc<F, Fut>(
    filter: &mut HavocFilter,
    msg: ServerMessage,
    sim: SimClock,
    mut handle: F,
) where
    F: FnMut(ServerMessage) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    for (msg, delay) in filter.apply(msg) {
        sleep_havoc_delay(sim, delay).await;
        handle(msg).await;
    }
}

pub(crate) async fn flush_havoc<F, Fut>(filter: &mut HavocFilter, sim: SimClock, mut handle: F)
where
    F: FnMut(ServerMessage) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    for (msg, delay) in filter.flush() {
        sleep_havoc_delay(sim, delay).await;
        handle(msg).await;
    }
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
/// (AD17/AE19: a `request_*` fetch or an HTTP order dispatch spawned just before
/// disconnect otherwise keeps issuing HTTP requests, racing the HttpQuota, and
/// sends into a possibly-dropped sink after the client stopped). Already-finished
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
/// (a server config change or a later-added instrument) otherwise streams into
/// nothing with zero diagnostics. The drain (`emit_trade`, the quote arm) has
/// no HTTP handle to re-seed, so it can only surface the miss - the poll path,
/// which does have async/HTTP context, self-heals via `ensure_instrument`. The
/// dedup keeps a per-trade warn from flooding the log for a genuinely-missing
/// symbol; it is process-global, which is acceptable for a diagnostic that only
/// wants to fire on the transition into the black-holed state.
pub(crate) fn warn_missing_instrument_once(symbol: &str) {
    static WARNED: std::sync::OnceLock<Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    let mut warned = lock_recover(
        WARNED.get_or_init(|| Mutex::new(std::collections::HashSet::new())),
        "missing-instrument warn set",
    );
    if warned.insert(symbol.to_string()) {
        tracing::warn!(
            %symbol,
            "no instrument def for a streamed symbol; its data is black-holed \
             until the instrument is seeded (server config change or later-added \
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
/// separate store fed exclusively by `DataEvent::Instrument`, and broadarrow's
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
/// the Nautilus cache, so broadarrow's executor refuses EVERY bar for the
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
                "dropping instrument: unrepresentable (currency unknown to nautilus?); \
                 broadarrow will refuse every bar for this symbol"
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
        cache.insert(def.symbol.clone(), def);
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
            join_url(base, "instruments"),
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
    held: Option<ServerMessage>,
}

impl HavocFilter {
    pub(crate) fn from_client(client: &ClientHavoc) -> Self {
        Self {
            latency: client.latency,
            drop_prob: client.drop_prob,
            duplicate_prob: client.duplicate_prob,
            reorder_prob: client.reorder_prob,
            rng: client
                .seed
                .map_or_else(|| StdRng::from_rng(&mut rand::rng()), StdRng::seed_from_u64),
            held: None,
        }
    }

    fn apply(&mut self, msg: ServerMessage) -> Vec<(ServerMessage, Duration)> {
        let mut candidates = Vec::new();
        if let Some(held) = self.held.take() {
            candidates.push(msg);
            candidates.push(held);
        } else if self.draw(self.reorder_prob) {
            self.held = Some(msg);
            return Vec::new();
        } else {
            candidates.push(msg);
        }
        self.emit_candidates(candidates)
    }

    fn flush(&mut self) -> Vec<(ServerMessage, Duration)> {
        let Some(held) = self.held.take() else {
            return Vec::new();
        };
        self.emit_candidates(vec![held])
    }

    fn emit_candidates(
        &mut self,
        candidates: Vec<ServerMessage>,
    ) -> Vec<(ServerMessage, Duration)> {
        let mut out = Vec::new();
        for msg in candidates {
            if self.draw(self.drop_prob) {
                continue;
            }
            let delay = self.delay_for(&msg);
            out.push((msg.clone(), delay));
            if self.draw(self.duplicate_prob) {
                out.push((msg, delay));
            }
        }
        out
    }

    pub(crate) fn delay_for(&self, msg: &ServerMessage) -> Duration {
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
pub(crate) fn client_havoc(spec: &Option<HavocSpec>) -> ClientHavoc {
    spec.as_ref()
        .map_or_else(ClientHavoc::default, |spec| spec.client.clone())
}
pub(crate) fn data_regime(spec: &Option<HavocSpec>) -> Option<MarketRegime> {
    spec.as_ref().and_then(|spec| spec.data)
}
pub(crate) fn conn_havoc(spec: &Option<HavocSpec>) -> ConnHavoc {
    spec.as_ref()
        .map_or_else(ConnHavoc::default, |spec| spec.conn)
}
/// Wall floor for the scaled HTTP request timeout, in seconds. Unlike the other
/// scaled durations (whose floor is the ~1ms tokio granularity), this one guards
/// a REAL local-IO round trip whose wall cost does NOT compress with `speed`:
/// dividing a sim-seconds timeout by a high `speed` would otherwise yield a
/// sub-second wall budget that the actual HTTP round trip blows, spuriously
/// timing out every order. Clamping UP to one wall second keeps the request
/// survivable; the consequence is that `request_timeout_secs` is the tightest
/// contributor to the usable-speed ceiling. Documented in `reference/clock.md`.
const MIN_WALL_REQUEST_TIMEOUT_SECS: u64 = 1;

pub(crate) fn request_timeout_secs(spec: &Option<HavocSpec>, sim: SimClock) -> u64 {
    let configured = conn_havoc(spec).request_timeout_secs;
    let sim_secs = if configured == 0 {
        mogwai_protocol::DEFAULT_REQUEST_TIMEOUT_SECS
    } else {
        configured
    };
    let wall_ns = sim.wall_span(sim_secs.saturating_mul(1_000_000_000));
    wall_ns
        .div_ceil(1_000_000_000)
        .max(MIN_WALL_REQUEST_TIMEOUT_SECS)
}
pub(crate) fn client_havoc_for_dispatch(spec: &Option<HavocSpec>, counter: u64) -> ClientHavoc {
    let mut client = client_havoc(spec);
    if let Some(seed) = client.seed.as_mut() {
        *seed ^= counter;
    }
    client
}
pub(crate) async fn sleep_havoc_delay(sim: SimClock, delay: Duration) {
    if !delay.is_zero() {
        tokio::time::sleep(sim.wall_duration(duration_to_nanos(delay))).await;
    }
}
/// Wall backoff between clock-fetch retries. Small and fixed: this runs inline
/// in connect(), so it must not stall boot for long, but a couple of retries
/// ride out a transient blip before the identity fallback commits the whole
/// connection to the wrong time axis.
const CLOCK_FETCH_RETRY_DELAY: Duration = Duration::from_millis(200);
const CLOCK_FETCH_MAX_ATTEMPTS: u32 = 3;

pub(crate) async fn fetch_clock_or_identity(http: &HttpClient, http_base: &str) -> ServerClock {
    // Retry before committing to identity (AD16): the identity fallback silently
    // puts EVERY ts_init, havoc sleep, quota interval, backoff and timeout on the
    // wrong axis for the life of the connection if the server actually runs at
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
                return clock;
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
         clock - if the server runs at speed != 1, every ts_init, havoc sleep, \
         quota interval, backoff and timeout will be scaled on the wrong axis for \
         the life of this connection"
    );
    // No reachable server: identity map and an UNKNOWN tape floor. A
    // `data_origin_ns` of 0 is the "floor unknown" sentinel the warmup
    // guard checks - it skips the pre-flight refusal and lets the server's
    // own 422 stand if a later request is off-tape.
    ServerClock {
        sim: SimClock::identity(),
        server_now_ns: 0,
        data_origin_ns: 0,
        backfill_horizon_ns: 0,
    }
}
pub(crate) fn duration_to_nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}
/// Resolves the effective row limit sent to the server's bounded `/trades`
/// scan. A missing limit defaults to the ceiling, and any requested limit is
/// clamped to it so neither the response body nor the materialized nautilus
/// response `Vec` can grow unbounded over a multi-GB dump.
pub(crate) fn capped_limit(limit: Option<std::num::NonZeroUsize>) -> usize {
    limit
        .map_or(
            mogwai_protocol::MAX_HISTORY_LIMIT,
            std::num::NonZeroUsize::get,
        )
        .min(mogwai_protocol::MAX_HISTORY_LIMIT)
}
pub(crate) fn join_url(base: &str, path: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), path)
}
pub(crate) fn symbol_from_instrument(instrument_id: InstrumentId) -> String {
    instrument_id.symbol.to_string()
}
/// Maps an optional request bound onto the u64 nanosecond axis, SATURATING
/// out-of-range datetimes at the axis bounds instead of dropping them to
/// `None`. `None` means "unbounded" to every caller, so silently mapping a
/// pre-epoch end to `None` would widen the request (an end of 1950 becoming
/// "until forever") and a post-2262 start would become "from origin"; clamping
/// preserves the requester's intent (a pre-epoch bound is the epoch, a
/// far-future bound is the axis ceiling) and lets a nonsense range come back
/// loudly empty rather than quietly full.
pub(crate) fn date_to_unix_nanos(date: Option<chrono::DateTime<chrono::Utc>>) -> Option<UnixNanos> {
    date.map(|dt| {
        // `timestamp_nanos_opt` is `None` outside ~1677..=2262; pick the side
        // by comparing against the epoch, then clamp negatives to zero.
        let ns = dt.timestamp_nanos_opt().unwrap_or_else(|| {
            if dt >= chrono::DateTime::<chrono::Utc>::UNIX_EPOCH {
                i64::MAX
            } else {
                i64::MIN
            }
        });
        UnixNanos::from(u64::try_from(ns).unwrap_or(0))
    })
}
/// Refuse an off-tape warmup BEFORE spending a round trip on it. A `start`
/// below the published `data_origin` can never be served (the tape begins at the
/// origin), so a fetch would come back an empty `200` the warmup cannot tell
/// from "no trades happened" - or, post-Landing-2, a server `422`. Failing here,
/// naming both the requested start and the floor, turns that into a loud,
/// surfaced error at the request boundary instead of a silent doomed fetch.
///
/// `data_origin == 0` is the "floor unknown" sentinel (the `/clock` fetch failed
/// and the client fell back to identity): the check is skipped so the server's
/// own refusal stays authoritative. A `None` start means "from origin" and is
/// always on-tape.
pub(crate) fn ensure_on_tape(start: Option<UnixNanos>, data_origin: u64) -> anyhow::Result<()> {
    if let Some(start) = start
        && data_origin != 0
        && start.as_u64() < data_origin
    {
        anyhow::bail!(
            "requested start {} precedes data_origin_ns {}; the mogwai tape cannot serve before its origin",
            start.as_u64(),
            data_origin
        );
    }
    Ok(())
}
pub(crate) fn now_unix_nanos(sim: SimClock) -> UnixNanos {
    // Thin typed wrapper over the shared wall read plus the fetched simulated
    // clock. The underlying reader keeps the saturating contract; the affine
    // map then places adapter-side `ts_init` on the same axis as the server.
    UnixNanos::from(sim.sim_ns(mogwai_protocol::now_unix_nanos()))
}
pub(crate) async fn wait_connected(
    connected: &Arc<AtomicBool>,
    ws_url: &str,
) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        if connected.load(Ordering::Relaxed) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    anyhow::bail!("connect websocket {ws_url} timed out")
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    #[test]
    fn date_to_unix_nanos_maps_in_range_datetimes_exactly() {
        let dt = chrono::Utc.timestamp_nanos(1_234_567_890_123_456_789);
        assert_eq!(
            date_to_unix_nanos(Some(dt)),
            Some(UnixNanos::from(1_234_567_890_123_456_789u64))
        );
        assert_eq!(date_to_unix_nanos(None), None);
    }

    #[test]
    fn date_to_unix_nanos_saturates_out_of_range_instead_of_unbounding() {
        // Pre-epoch (in i64-nanos range but negative): clamps to the epoch, so
        // an end of 1950 stays an empty range rather than becoming "forever".
        let pre_epoch = chrono::Utc.with_ymd_and_hms(1950, 1, 1, 0, 0, 0).unwrap();
        assert_eq!(
            date_to_unix_nanos(Some(pre_epoch)),
            Some(UnixNanos::from(0u64))
        );

        // Pre-1677 (outside i64-nanos range entirely): same clamp to epoch.
        let ancient = chrono::Utc.with_ymd_and_hms(1500, 1, 1, 0, 0, 0).unwrap();
        assert_eq!(
            date_to_unix_nanos(Some(ancient)),
            Some(UnixNanos::from(0u64))
        );

        // Post-2262: clamps to the axis ceiling, so a far-future start stays a
        // loud empty range rather than becoming "from origin".
        let far_future = chrono::Utc.with_ymd_and_hms(3000, 1, 1, 0, 0, 0).unwrap();
        assert_eq!(
            date_to_unix_nanos(Some(far_future)),
            Some(UnixNanos::from(u64::try_from(i64::MAX).expect("fits")))
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
        let mut filter = HavocFilter::from_client(&ClientHavoc::default());
        let per_msg = filter.delay_for(&ServerMessage::Heartbeat { ts_event: 0 });
        assert!(
            !per_msg.is_zero(),
            "baseline latency must be nonzero for this test"
        );

        let (deliver_tx, deliver_rx) = tokio::sync::mpsc::unbounded_channel::<HavocDelivery>();
        let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<ServerMessage>();
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
                ServerMessage::Heartbeat { ts_event: i },
                sim,
                &deliver_tx,
            );
        }
        drop(deliver_tx);

        for expected in 0..N {
            let got = out_rx.recv().await.expect("every message is delivered");
            let ServerMessage::Heartbeat { ts_event } = got else {
                panic!("unexpected message on the pump output");
            };
            assert_eq!(ts_event, expected, "the pump preserves arrival order");
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed < per_msg * 3,
            "the burst drained in {elapsed:?}; a serial drain would need ~{:?}",
            per_msg * u32::try_from(N).unwrap()
        );
        drop(pump);
    }

    #[tokio::test]
    async fn drain_havoc_anchored_does_not_compound_a_page() {
        // The poll path anchors a whole fetched page at one arrival instant, so
        // the per-message latency is paid once for the page, not once per trade
        // (AD4). Draining N messages against a single anchor must take ~one delay
        // window, and deliver them in order.
        let sim = SimClock::identity();
        let mut filter = HavocFilter::from_client(&ClientHavoc::default());
        let per_msg = filter.delay_for(&ServerMessage::Heartbeat { ts_event: 0 });

        let arrival = Instant::now();
        const N: u64 = 30;
        let mut delivered: Vec<u64> = Vec::new();
        let start = Instant::now();
        for i in 0..N {
            drain_havoc_anchored(
                &mut filter,
                ServerMessage::Heartbeat { ts_event: i },
                sim,
                arrival,
                |msg| {
                    if let ServerMessage::Heartbeat { ts_event } = msg {
                        delivered.push(ts_event);
                    }
                    std::future::ready(())
                },
            )
            .await;
        }
        let elapsed = start.elapsed();

        assert_eq!(
            delivered,
            (0..N).collect::<Vec<_>>(),
            "the page drains in order"
        );
        assert!(
            elapsed < per_msg * 3,
            "the page drained in {elapsed:?}; per-trade compounding would need ~{:?}",
            per_msg * u32::try_from(N).unwrap()
        );
    }
}
