//! HTTP surface: shared app state plus every plain request/response route
//! (`/instruments`, `/account`, `/clock`, `/trades`, `/quotes`, `/orders`,
//! `/control/divergence`). The stateful, streaming websocket surface
//! (`/ws`) lives in `ws.rs`; both share `AppState` and the order-entry
//! validation gate (`process_order_cmd`) defined here.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use mogwai_data::TickEvent;
use mogwai_engine::Engine;
use mogwai_protocol::{
    AccountState, ClientMessage, InstrumentDef, MAX_HISTORY_LIMIT, MarketRegime, OrderType,
    QuoteTick, ServerClock, ServerMessage, SimClock, TradeTick, control::Divergence,
    validate_divergence, validate_market_regime, validate_modify_order, validate_submit_order,
};
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::config::{Config, sim_now_ns, window_until_ns};
use crate::source;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) engine: Arc<Mutex<Engine>>,
    pub(crate) cfg: Config,
    pub(crate) profiles: Arc<source::InstrumentProfiles>,
    /// Run clock shared with adapters over `/clock`.
    pub(crate) sim: SimClock,
    /// Earliest sim-time unix-ns instant the synthetic tape can serve, derived
    /// once at boot as `sim.sim_ns(now) - backfill_horizon_ns`. The binding floor
    /// for `/trades` and the anchor both source builders generate from, so the
    /// data origin and the advertised clock cannot diverge.
    pub(crate) data_origin_ns: u64,
    /// Execution-event delay in milliseconds, shared by all live writers.
    pub(crate) delay_ms: Arc<AtomicU64>,
    /// Sim-time unix-ns instant before which writers drop all outbound frames.
    pub(crate) dark_until_ns: Arc<AtomicU64>,
    /// Sim-time unix-ns instant before which writers drop market-data frames.
    pub(crate) stall_until_ns: Arc<AtomicU64>,
}

/// Control plane: arm a divergence to fire on its next trigger.
pub(crate) async fn arm_divergence(
    State(state): State<AppState>,
    Json(div): Json<Divergence>,
) -> impl IntoResponse {
    tracing::info!(?div, "arming divergence");
    // Validate at the arming boundary so an out-of-range knob (e.g. a
    // `PartialFillNext.fraction` outside `(0, 1]`) is rejected before it is
    // stored into server state or armed on the engine, rather than surfacing
    // as a degenerate fill downstream. Mirrors the `validate_market_regime`
    // gate on the subscription/history paths.
    if let Err(err) = validate_divergence(&div) {
        tracing::warn!(?div, err, "rejecting out-of-range divergence");
        return (StatusCode::BAD_REQUEST, err.to_string());
    }
    match div {
        Divergence::DelayAcks { ms } => {
            state.delay_ms.store(ms, Ordering::Relaxed);
        }
        // GoDark/StallData windows are STORE-not-extend (S18): each arm overwrites
        // the absolute deadline with `now + ms`, so re-arming with a SMALLER `ms`
        // shortens an in-flight blackout rather than lengthening it. This is
        // deliberate - re-arming sets the window, it does not accumulate - and lets
        // a test cut a window short by re-posting a small one; an operator wanting a
        // longer window re-arms with the longer `ms`.
        Divergence::GoDark { ms } => {
            state.dark_until_ns.store(
                window_until_ns(sim_now_ns(state.sim), ms),
                Ordering::Relaxed,
            );
        }
        Divergence::StallData { ms } => {
            state.stall_until_ns.store(
                window_until_ns(sim_now_ns(state.sim), ms),
                Ordering::Relaxed,
            );
        }
        Divergence::ClearDivergences => {
            // Lift both server-owned temporal windows process-wide. `0` is the
            // cleared sentinel: `delay_ms == 0` skips the exec pump's delay sleep,
            // and `now_ns() < 0` is never true so the dark and data-stall
            // guards are off. There is no backlog to replay because gated
            // frames are dropped.
            state.delay_ms.store(0, Ordering::Relaxed);
            state.dark_until_ns.store(0, Ordering::Relaxed);
            state.stall_until_ns.store(0, Ordering::Relaxed);
        }
        // Server-ownership contract (pins B.4 / E.11): `DelayAcks`, `GoDark`,
        // `StallData`, and `ClearDivergences` are server-owned controls with no
        // synchronous engine-side trigger. The server owns them and must NEVER
        // forward them to `engine.arm()`.
        // The arms above intercept them before this catch-all, so `engine_div`
        // can only be one of the four engine-side variants. The assert makes a
        // future refactor that forwards a whole `HavocSpec.server` vec straight
        // to `engine.arm()` fail loudly rather than silently losing these knobs.
        engine_div => {
            debug_assert!(
                !matches!(
                    engine_div,
                    Divergence::DelayAcks { .. }
                        | Divergence::GoDark { .. }
                        | Divergence::StallData { .. }
                        | Divergence::ClearDivergences
                ),
                "DelayAcks/GoDark/StallData/ClearDivergences are server-owned and must not be forwarded to engine.arm()",
            );
            state.engine.lock().await.arm(engine_div);
        }
    }
    (StatusCode::ACCEPTED, String::new())
}

pub(crate) async fn instruments(State(state): State<AppState>) -> Json<Vec<InstrumentDef>> {
    Json(state.engine.lock().await.instrument_defs())
}

/// Pull route for the venue's current account snapshot.
///
/// AccountState is execution-owned and is otherwise only pushed with an order
/// event. An adapter connecting over either transport pulls this once so the
/// bridge's account row exists before the first order is worked, rather than
/// learning the account only when the first fill's AccountState arrives. The
/// route is transport-agnostic, so it also serves the HttpOrders execution
/// profile that opens no /ws socket and never sends Subscribe.
pub(crate) async fn account(State(state): State<AppState>) -> Json<AccountState> {
    let ts = sim_now_ns(state.sim);
    Json(state.engine.lock().await.account_snapshot(ts))
}

pub(crate) async fn clock(State(state): State<AppState>) -> Json<ServerClock> {
    // Publish the tape boundary alongside the affine map so a client can guard
    // its own warmup against `data_origin_ns` rather than issuing a doomed
    // off-tape fetch. `server_now_ns` is sampled here so the client gets sim-now
    // and the floor from one round trip, without reading its own (skewable) wall
    // clock. `data_origin_ns` is the boot-derived floor; the horizon is echoed so
    // the client can report the floor in its own terms.
    Json(ServerClock {
        sim: state.sim,
        server_now_ns: sim_now_ns(state.sim),
        data_origin_ns: state.data_origin_ns,
        backfill_horizon_ns: state.cfg.backfill_horizon_ns,
    })
}

/// A wire MARKET order carries no price (mirroring Nautilus, which never
/// stamps one), but the engine has no book of its own and fills "at the
/// order's own price" - so before either order-entry surface hands a
/// `SubmitOrder` to the engine, stamp a MARKET order missing a price with the
/// venue's own current synthesized price. A LIMIT order's price is left
/// untouched: the engine's "submit price required" rejection is correct for a
/// limit, it only over-reached for the price-less market case.
///
/// Synthesis (`positioned_generator` -> `source_at_or_before`) locks the
/// symbol's own checkpoint mutex and, on a cold/deep index, walks up to
/// `MAX_HISTORY_SEEK_TICKS`. `/trades` already pushes the identical synthesis
/// onto `spawn_blocking` rather than running it inline on the tokio worker
/// (see `trades` above); this does the same, so a burst of price-less market
/// orders cannot stall the runtime's worker pool or serialize behind a seeked
/// `/trades` request (or vice versa) any longer than the symbol's shared
/// index itself requires - other symbols' requests do not queue here at all
/// (S13).
async fn stamp_market_price(msg: ClientMessage, state: &AppState) -> ClientMessage {
    let ClientMessage::SubmitOrder(mut order) = msg else {
        return msg;
    };
    if order.order_type == OrderType::Market && order.price.is_none() {
        let symbol = order.symbol.clone();
        let profiles = Arc::clone(&state.profiles);
        let data_origin = state.data_origin_ns;
        let sim_now = sim_now_ns(state.sim);
        order.price = match tokio::task::spawn_blocking(move || {
            source::current_price(&symbol, &profiles, data_origin, sim_now)
        })
        .await
        {
            Ok(price) => price,
            Err(e) => {
                tracing::error!(%e, "market price synthesis task failed");
                None
            }
        };
    }
    ClientMessage::SubmitOrder(order)
}

/// Run one order-entry command (`SubmitOrder`/`ModifyOrder`/`CancelOrder`)
/// through the protocol-boundary validators before it ever reaches the
/// engine, the same way `arm_divergence` calls `validate_divergence` and the
/// subscription/history paths call `validate_market_regime` before touching
/// server or engine state. A `SubmitOrder` with a non-positive quantity or a
/// priceless limit, or a `ModifyOrder` that sets neither field, is rejected
/// right here as a synthesized `OrderRejected`/`OrderModifyRejected` - never
/// reaching `stamp_market_price` or the engine mutex - instead of leaning on
/// the engine's own (correct, but redundant) defensive checks. `CancelOrder`
/// has no protocol-boundary validator and passes straight through. Shared by
/// both order-entry surfaces (`POST /orders` and the `/ws` handling below) so
/// the gate lives in exactly one place.
pub(crate) async fn process_order_cmd(
    order_cmd: ClientMessage,
    state: &AppState,
) -> Vec<ServerMessage> {
    // Sampled at entry for the protocol-boundary rejections below: they return
    // before any price synthesis, so entry-time is when they logically occur.
    let ts = sim_now_ns(state.sim);
    match &order_cmd {
        ClientMessage::SubmitOrder(order) => {
            if let Err(reason) = validate_submit_order(order) {
                return vec![ServerMessage::OrderRejected {
                    client_order_id: order.client_order_id.clone(),
                    reason: reason.to_string(),
                    ts_event: ts,
                }];
            }
        }
        ClientMessage::ModifyOrder {
            client_order_id,
            price,
            quantity,
        } => {
            if let Err(reason) = validate_modify_order(*price, *quantity) {
                return vec![ServerMessage::OrderModifyRejected {
                    client_order_id: client_order_id.clone(),
                    venue_order_id: None,
                    reason: reason.to_string(),
                    ts_event: ts,
                }];
            }
        }
        ClientMessage::CancelOrder { .. } => {}
        ClientMessage::Subscribe { .. } | ClientMessage::Unsubscribe { .. } => {
            unreachable!("callers route Subscribe/Unsubscribe away before process_order_cmd")
        }
    }
    let order_cmd = stamp_market_price(order_cmd, state).await;
    // Re-sample after the market-price synthesis, which for a price-less MARKET
    // order may block ~100 ms on the checkpoint mutex and seek. Stamping the
    // engine's events with the entry-time `ts` would date them up to a seek's
    // worth of sim-time before they logically occur - ~10 sim-seconds at speed
    // 100 (S16). The synthesis-failure reject and the engine events below all
    // occur now, after synthesis, so they take this fresh instant.
    let ts = sim_now_ns(state.sim);
    // A MARKET order still price-less after the stamp, for a symbol this venue
    // DOES list, means `current_price` failed: the positioning seek could not
    // reach sim-now within its budget (or the synthesis task itself died).
    // Reject it here with the honest story - the client correctly sent no
    // price (Nautilus never stamps a market order), so letting the engine's
    // "submit price required" fire would blame the client for the venue's own
    // synthesis failure. An UNCONFIGURED symbol is deliberately left
    // price-less: the engine checks instrument existence before the price, so
    // its "unknown instrument" rejection tells that story unaltered.
    if let ClientMessage::SubmitOrder(order) = &order_cmd
        && order.order_type == OrderType::Market
        && order.price.is_none()
        && state.profiles.get(&order.symbol).is_some()
    {
        tracing::warn!(
            symbol = %order.symbol,
            client_order_id = %order.client_order_id,
            "rejecting price-less market order: market price synthesis failed"
        );
        return vec![ServerMessage::OrderRejected {
            client_order_id: order.client_order_id.clone(),
            reason: "venue could not synthesize a market price at sim-now".to_string(),
            ts_event: ts,
        }];
    }
    state.engine.lock().await.process(order_cmd, ts)
}

/// HTTP order-entry surface for profiles that do not use `/ws` for orders.
pub(crate) async fn submit_order_http(
    State(state): State<AppState>,
    Json(msg): Json<ClientMessage>,
) -> Result<Json<Vec<ServerMessage>>, StatusCode> {
    match msg {
        ClientMessage::Subscribe { .. } | ClientMessage::Unsubscribe { .. } => {
            Err(StatusCode::BAD_REQUEST)
        }
        order_cmd => Ok(Json(process_order_cmd(order_cmd, &state).await)),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct HistoryQuery {
    pub(crate) symbol: String,
    pub(crate) start: Option<u64>,
    pub(crate) end: Option<u64>,
    pub(crate) limit: Option<usize>,
    #[serde(default)]
    pub(crate) regime: Option<String>,
}

pub(crate) async fn trades(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<HistoryQuery>,
) -> Result<Json<Vec<TradeTick>>, (StatusCode, String)> {
    let limit = normalize_limit(query.limit);
    let mut regime = parse_history_regime(query.regime.as_deref());
    strip_unfireable_reopen_gap(&mut regime, state.data_origin_ns);
    let profiles = Arc::clone(&state.profiles);
    let data_origin = state.data_origin_ns;
    let sim_now = sim_now_ns(state.sim);
    let HistoryQuery {
        symbol, start, end, ..
    } = query;

    // Analytic refuse of an off-tape window: a `start` before the data origin can
    // never be served (the tape begins at `data_origin`), so reject it LOUDLY with
    // the floor named rather than draining the seek cap and returning an empty
    // `200` the warmup cannot distinguish from "no trades happened". `None` means
    // "from origin" and is served; degenerate windows (start > end, limit 0) flow
    // through to `bounded_trades` unchanged.
    if let Some(start) = start
        && start < data_origin
    {
        let body = format!(
            "requested start {start} precedes data_origin_ns {data_origin}; the tape cannot serve before its origin"
        );
        tracing::warn!(start, data_origin, "refusing off-tape trades window");
        return Err((StatusCode::UNPROCESSABLE_ENTITY, body));
    }

    // The symmetric ceiling: tape past sim-now does not exist yet. Every
    // legitimate window lives in `[data_origin, sim_now]` by construction, and
    // the generator is deterministic, so serving a future `start` would extend
    // the shared index past the clock and hand the client tomorrow's tape - a
    // look-ahead leak no real venue can produce. Refused with the same loud
    // `422` as the origin floor, so "you asked for data that cannot exist yet"
    // stays distinguishable from "no trades happened".
    if let Some(start) = start
        && start > sim_now
    {
        let body = format!(
            "requested start {start} exceeds sim-now {sim_now}; the tape cannot serve past the clock"
        );
        tracing::warn!(start, sim_now, "refusing future trades window");
        return Err((StatusCode::UNPROCESSABLE_ENTITY, body));
    }

    // Clamp the served tail at sim-now for the same reason: an `end` past the
    // clock - or a no-`end` request, which otherwise grinds out the full
    // `limit` however far into the future that lands - must not stream ticks
    // stamped ahead of the clock. The bound is inclusive, so a tick landing
    // exactly at sim-now is still served.
    let end = Some(end.map_or(sim_now, |end| end.min(sim_now)));

    // Synthesizing up to `MAX_HISTORY_LIMIT` ticks is pure CPU work against the
    // generator (the source never blocks on IO), and `next_tick` never returns
    // `None`, so a request grinds until it fills `limit` or crosses the
    // effective `end`. Run it on a blocking thread rather than inline on the
    // tokio worker, so a burst of `/trades` requests cannot stall the async
    // runtime's worker pool.
    let ticks = match tokio::task::spawn_blocking(move || {
        bounded_trades(&symbol, start, end, limit, regime, &profiles, data_origin)
    })
    .await
    {
        Ok(ticks) => ticks,
        Err(e) => {
            tracing::error!(%e, "history synthesis task failed");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, String::new()));
        }
    };
    Ok(Json(ticks))
}

pub(crate) async fn quotes(
    axum::extract::Query(_query): axum::extract::Query<HistoryQuery>,
) -> Json<Vec<QuoteTick>> {
    // Mogwai's generated history is trades-only, so a bounded historical quote
    // fetch is empty by construction. If synthesized top-of-book is wired in,
    // this route grows the same seek-and-bound scan as `trades`.
    Json(Vec::new())
}

/// The single owner of `/trades` page-size policy: a missing `limit` requests a
/// full page, and any requested size is clamped to `MAX_HISTORY_LIMIT`. Folding
/// the `unwrap_or` + `.min()` here (rather than splitting the clamp into the
/// handler and an empty-vec guard into `bounded_trades`) keeps the page-size
/// contract in one place. A normalized `0` still flows through to the early
/// return in `bounded_trades`, which the synthesis loop relies on (the
/// `out.len() >= limit` break would otherwise yield one tick for `limit == 0`).
pub(crate) fn normalize_limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(MAX_HISTORY_LIMIT).min(MAX_HISTORY_LIMIT)
}

pub(crate) fn bounded_trades(
    symbol: &str,
    start: Option<u64>,
    end: Option<u64>,
    limit: usize,
    regime: Option<MarketRegime>,
    profiles: &source::InstrumentProfiles,
    data_origin: u64,
) -> Vec<TradeTick> {
    if limit == 0 {
        return Vec::new();
    }

    let Some(mut merged) =
        source::build_history_source(symbol, start, regime, profiles, data_origin)
    else {
        return Vec::new();
    };
    let mut out = Vec::new();

    while let Some(tick) = merged.next_tick() {
        if let Some(end) = end
            && tick.ts_event() > end
        {
            break;
        }

        let TickEvent::Trade(trade) = tick else {
            continue;
        };
        out.push(trade);
        if out.len() >= limit {
            break;
        }
    }

    out
}

pub(crate) fn parse_history_regime(raw: Option<&str>) -> Option<MarketRegime> {
    let raw = raw?;
    let Ok(regime) = serde_json::from_str::<MarketRegime>(raw) else {
        tracing::warn!(raw, "dropping malformed market regime");
        return None;
    };
    validate_regime_or_clean(Some(regime))
}

/// D3's API-boundary half: a `ReopenGap` whose `at_ts` sits at or before the
/// tape origin can never fire - the generator consumes it at construction
/// with a warning and the realized tape is byte-identical to clean - so
/// decide the degradation loudly here at the boundary instead of deep in the
/// generator. Stripping also keeps the request on the checkpoint-index fast
/// path: any `Some(regime)` bypasses the shared index (a regime'd realization
/// is a different tape), so a doomed gap would otherwise buy the slow
/// from-origin drain for a tape identical to clean. Returns the stripped
/// `at_ts` so the WS carrier can announce the strip on the wire; the HTTP
/// history path has no diagnostic side channel and ignores it.
pub(crate) fn strip_unfireable_reopen_gap(
    regime: &mut Option<MarketRegime>,
    data_origin_ns: u64,
) -> Option<u64> {
    if let Some(MarketRegime::ReopenGap { at_ts, .. }) = *regime
        && at_ts <= data_origin_ns
    {
        tracing::warn!(
            at_ts,
            data_origin_ns,
            "ReopenGap at or before the tape origin can never fire; serving the clean tape"
        );
        *regime = None;
        return Some(at_ts);
    }
    None
}

pub(crate) fn validate_regime_or_clean(regime: Option<MarketRegime>) -> Option<MarketRegime> {
    let regime = regime?;
    match validate_market_regime(&regime) {
        Ok(()) => Some(regime),
        Err(err) => {
            tracing::warn!(?regime, err, "dropping out-of-range market regime");
            None
        }
    }
}
