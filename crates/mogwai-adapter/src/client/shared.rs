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
    time::Duration,
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
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;

use crate::{clock::fetch_clock, convert, lifecycle::HttpQuota};

/// Drains the havoc-mangled expansion of one inbound `msg` and routes each
/// resulting wire message through `handle`, sleeping the per-message delay
/// first. Generic over the per-message sink so the market path (which forwards
/// to `handle_market_message`, async) and the exec path (which forwards to
/// `handle_exec_message`, wrapped in an async block) share one control flow.
/// `flush_havoc` is the same loop over `filter.flush()` for the disconnect
/// teardown that emits any divergence-held events.
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
pub(crate) fn date_to_unix_nanos(date: Option<chrono::DateTime<chrono::Utc>>) -> Option<UnixNanos> {
    date.and_then(|dt| dt.timestamp_nanos_opt())
        .and_then(|ns| u64::try_from(ns).ok())
        .map(UnixNanos::from)
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
