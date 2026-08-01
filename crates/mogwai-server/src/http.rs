// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

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
use mogwai_engine::{Engine, MAX_ARMED_DIVERGENCES};
use mogwai_protocol::{
    AccountState, AdmissionSubject, ClientMessage, InstrumentDef, MAX_HISTORY_LIMIT, MarketRegime,
    OrderType, QuoteTick, ServerClock, ServerMessage, SimClock, TradeTick, control::Divergence,
    truncate_client_id, truncate_reason, validate_client_order_id, validate_divergence,
    validate_market_regime, validate_modify_order, validate_request_id, validate_submit_order,
};
use serde::Deserialize;
use tokio::sync::{Mutex, Semaphore};

use crate::admission::{ExecLanes, Reservation};
use crate::config::{Config, sim_now_ns, window_until_ns};
use crate::source;

/// What one order-entry command came to. Every variant that carries frames also
/// carries the reservation those frames were produced under: there is no path
/// where a frame exists against no reservation.
pub(crate) enum OrderOutcome {
    /// The engine processed the command and produced these events.
    Produced {
        events: Vec<ServerMessage>,
        reservation: Reservation,
    },
    /// The protocol boundary refused it before the engine ever saw it. These
    /// are engine-free frames and are charged like any other output.
    Refused {
        events: Vec<ServerMessage>,
        reservation: Reservation,
    },
    /// Admission refused: outbound capacity could not cover the worst case, so
    /// the engine was never asked. Carries the frame for the PRIORITY lane.
    NotAdmitted(ServerMessage),
    /// A malformed request with no order-shaped frame to answer it (a query
    /// whose `request_id` is over-length names no order), so the answer is the
    /// untargeted diagnostic. Also a priority-lane frame, but deliberately NOT
    /// an `AdmissionRejected`: conflating malformed with over-capacity would
    /// make an admission refusal unreadable as a load signal.
    Diagnostic(ServerMessage),
}

/// Name what a refusal refused, so the consumer can translate it per command -
/// a refused cancel must not read as a rejected order.
fn admission_subject(cmd: &ClientMessage) -> AdmissionSubject {
    match cmd {
        ClientMessage::SubmitOrder(o) => AdmissionSubject::Submit {
            client_order_id: o.client_order_id.clone(),
        },
        ClientMessage::CancelOrder { client_order_id } => AdmissionSubject::Cancel {
            client_order_id: client_order_id.clone(),
        },
        ClientMessage::ModifyOrder {
            client_order_id, ..
        } => AdmissionSubject::Modify {
            client_order_id: client_order_id.clone(),
        },
        ClientMessage::QueryOrders { request_id, .. } => AdmissionSubject::Query {
            request_id: request_id.clone(),
            query: mogwai_protocol::QueryKind::Orders,
        },
        ClientMessage::QueryFills { request_id, .. } => AdmissionSubject::Query {
            request_id: request_id.clone(),
            query: mogwai_protocol::QueryKind::Fills,
        },
        _ => AdmissionSubject::Frame,
    }
}

/// The protocol-boundary verdict on one order-entry command: `Some(reason)`
/// when it is malformed. Ids are length-checked here, alongside the numeric
/// checks that were always here, because both are malformed-request failures.
fn boundary_error(cmd: &ClientMessage) -> Option<&'static str> {
    match cmd {
        ClientMessage::SubmitOrder(order) => validate_submit_order(order).err(),
        ClientMessage::ModifyOrder {
            client_order_id,
            price,
            quantity,
        } => validate_client_order_id(client_order_id)
            .err()
            .or_else(|| validate_modify_order(*price, *quantity).err()),
        // A cancel's only failure modes are venue-side, and a query is
        // answered truthfully whatever it asks (an unknown id is an empty
        // snapshot, not an error) - so for these the id caps are the whole
        // boundary check.
        ClientMessage::CancelOrder { client_order_id } => {
            validate_client_order_id(client_order_id).err()
        }
        ClientMessage::QueryOrders {
            request_id,
            client_order_id,
            ..
        }
        | ClientMessage::QueryFills {
            request_id,
            client_order_id,
            ..
        } => validate_request_id(request_id).err().or_else(|| {
            client_order_id
                .as_ref()
                .and_then(|id| validate_client_order_id(id).err())
        }),
        ClientMessage::Subscribe { .. } | ClientMessage::Unsubscribe { .. } => {
            unreachable!("callers route Subscribe/Unsubscribe away before process_order_cmd")
        }
    }
}

/// The refusal frame for a malformed order-entry command, echoing the offending
/// id TRUNCATED to `MAX_CLIENT_ID_LEN`. Echoing it at full length would turn an
/// 8 MiB `client_order_id` into an 8 MiB `OrderRejected`, recreating exactly
/// the unbounded frame the cap exists to prevent; a truncated echo cannot be
/// mistaken for a live correlation because the venue would never have accepted
/// the id under either spelling, and the reason says so.
fn boundary_refusal(cmd: &ClientMessage, reason: &str, ts: u64) -> ServerMessage {
    let note = |id: &str| {
        if id.len() > mogwai_protocol::MAX_CLIENT_ID_LEN {
            format!(
                "{reason}; the identifier was truncated for display and no order \
                 exists under either spelling"
            )
        } else {
            reason.to_string()
        }
    };
    match cmd {
        ClientMessage::ModifyOrder {
            client_order_id, ..
        } => ServerMessage::OrderModifyRejected {
            client_order_id: truncate_client_id(client_order_id.clone()),
            venue_order_id: None,
            reason: note(client_order_id),
            ts_event: ts,
        },
        ClientMessage::CancelOrder { client_order_id } => ServerMessage::OrderCancelRejected {
            client_order_id: truncate_client_id(client_order_id.clone()),
            venue_order_id: None,
            reason: note(client_order_id),
            ts_event: ts,
        },
        ClientMessage::SubmitOrder(order) => ServerMessage::OrderRejected {
            client_order_id: truncate_client_id(order.client_order_id.clone()),
            reason: note(&order.client_order_id),
            ts_event: ts,
        },
        // Queries have no order-shaped rejection frame; the caller answers
        // these with the untargeted diagnostic instead.
        _ => ServerMessage::ProtocolError {
            reason: reason.to_string(),
            ts_event: ts,
        },
    }
}

/// Run one order-entry command (`SubmitOrder`/`ModifyOrder`/`CancelOrder`, and
/// the two venue-truth queries) through the protocol-boundary validators AND
/// admission control before the engine is allowed to mutate.
///
/// The order of the body is load-bearing:
///
/// 1. Validate ids and numerics. A failure refuses here, with its own
///    fixed-size reservation - a boundary refusal is still a produced frame.
/// 2. `stamp_market_price` (which may block on the checkpoint mutex), then
///    re-sample `ts`: stamping the engine's events with the entry-time `ts`
///    would date them up to a seek's worth of sim-time before they logically
///    occur.
/// 3. The post-stamp synthesis-failure refusal (a MARKET order still priceless
///    for a symbol this venue DOES list), which is the venue's own failure and
///    is not the engine's to blame on the client.
/// 4. Take the engine lock, read `book_shape()`, reserve worst-case output.
///    The lock spans the shape read and the processing, so the shape cannot
///    drift out from under the reservation that covers it.
/// 5. Only then `engine.process`. If the reservation failed, the engine was
///    never asked, so nothing mutated: no venue order id burned, no order
///    resting.
///
/// Shared by both order-entry surfaces (`POST /orders` and `/ws`) so the gate
/// lives in exactly one place; the HTTP surface passes per-request lanes.
pub(crate) async fn process_order_cmd(
    order_cmd: ClientMessage,
    state: &AppState,
    lanes: &ExecLanes,
) -> OrderOutcome {
    // Sampled at entry for the boundary rejections below: they return before
    // any price synthesis, so entry-time is when they logically occur.
    let ts = sim_now_ns(state.sim);
    if let Some(reason) = boundary_error(&order_cmd) {
        let Some(reservation) = lanes.try_reserve_boundary() else {
            return OrderOutcome::NotAdmitted(ServerMessage::AdmissionRejected {
                subject: admission_subject(&order_cmd),
                reason: "execution output admission budget exhausted".into(),
                ts_event: ts,
            });
        };
        let event = boundary_refusal(&order_cmd, reason, ts);
        if matches!(event, ServerMessage::ProtocolError { .. }) {
            // The reservation is not needed after all: nothing goes on the
            // held lane, so drop it and give the bytes straight back.
            drop(reservation);
            return OrderOutcome::Diagnostic(event);
        }
        return OrderOutcome::Refused {
            events: vec![event],
            reservation,
        };
    }
    let order_cmd = stamp_market_price(order_cmd, state).await;
    // Re-sample after the market-price synthesis, which for a price-less MARKET
    // order may block ~100 ms on the checkpoint mutex and seek (S16). The
    // synthesis-failure reject and the engine events below all occur now.
    let ts = sim_now_ns(state.sim);
    // A MARKET order still price-less after the stamp, for a symbol this venue
    // DOES list, means `current_price` failed: the positioning seek could not
    // reach sim-now within its budget (or the synthesis task itself died).
    // Reject it here with the honest story - the client correctly sent no price
    // (nautilus never stamps a market order), so letting the engine's "submit
    // price required" fire would blame the client for the venue's own synthesis
    // failure. An UNCONFIGURED symbol is deliberately left price-less: the
    // engine checks instrument existence before the price, so its "unknown
    // instrument" rejection tells that story unaltered.
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
        let Some(reservation) = lanes.try_reserve_boundary() else {
            return OrderOutcome::NotAdmitted(ServerMessage::AdmissionRejected {
                subject: admission_subject(&order_cmd),
                reason: "execution output admission budget exhausted".into(),
                ts_event: ts,
            });
        };
        return OrderOutcome::Refused {
            events: vec![ServerMessage::OrderRejected {
                client_order_id: order.client_order_id.clone(),
                reason: "venue could not synthesize a market price at sim-now".to_string(),
                ts_event: ts,
            }],
            reservation,
        };
    }
    let mut engine = state.engine.lock().await;
    let shape = engine.book_shape();
    let Some(reservation) = lanes.reserve(&order_cmd, &shape) else {
        // The engine has NOT been asked to process anything, so nothing
        // mutated: the refusal is the whole effect of this command.
        return OrderOutcome::NotAdmitted(ServerMessage::AdmissionRejected {
            subject: admission_subject(&order_cmd),
            reason: "execution output admission budget exhausted".into(),
            ts_event: ts,
        });
    };
    let events = engine.process(order_cmd, ts);
    drop(engine);
    OrderOutcome::Produced {
        events,
        reservation,
    }
}

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
    /// Global permit pool rationing concurrently-live per-symbol replay threads
    /// across every websocket connection (S22a). Each spawned replay holds one
    /// permit for its whole life and releases it when reaped, so a subscribe
    /// that cannot take a permit is refused for the over-limit symbols. Built
    /// from `Config::max_concurrent_replays`; a `0` cap yields a pool sized at
    /// `Semaphore::MAX_PERMITS`, i.e. never runs dry.
    pub(crate) replay_permits: Arc<Semaphore>,
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
    // The operator-supplied reject reason is truncated HERE, at the arming
    // boundary, before the divergence is stored: the engine echoes it verbatim
    // into `OrderRejected.reason`, so an uncapped one would make a produced
    // frame exceed the reservation sized against `ORDER_EVENT_MAX_BYTES` and
    // void the whole size model. Truncating at the boundary means the engine can
    // only ever echo an already-bounded string, and no engine change is needed.
    // Documented alongside the control in reference/havoc.md.
    let div = match div {
        Divergence::RejectNextSubmit { reason } => Divergence::RejectNextSubmit {
            reason: truncate_reason(reason),
        },
        other => other,
    };
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
        // Immediate book action, not an armed trigger: cancel the resting
        // order right now, silently (no lifecycle event - that lost event IS
        // the injected fault; the truth surfaces only via QueryOrders). A
        // miss - unknown id, or already terminal - is refused with a 404 so
        // a scenario cannot believe it armed a fault that never happened.
        Divergence::CancelOpenOrderSilently { client_order_id } => {
            let ts = sim_now_ns(state.sim);
            if let Err(reason) = state
                .engine
                .lock()
                .await
                .cancel_open_order_silently(&client_order_id, ts)
            {
                tracing::warn!(%client_order_id, reason, "refusing silent cancel");
                return (StatusCode::NOT_FOUND, reason);
            }
            tracing::info!(%client_order_id, "silently canceled resting order server-side");
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
                        | Divergence::CancelOpenOrderSilently { .. }
                ),
                "DelayAcks/GoDark/StallData/ClearDivergences/CancelOpenOrderSilently are server-owned or immediate and must not be forwarded to engine.arm()",
            );
            // Relay an eviction in the ack body. The queue is bounded
            // (`MAX_ARMED_DIVERGENCES`), and at the cap `arm` sheds the OLDEST
            // entry - so a bare `202` with an empty body would tell an armer
            // "accepted" while an earlier armed divergence it is still counting
            // on has just been discarded. That silence cost a QA run a full
            // misdiagnosis: a bracket of arms was posted, the order it targeted
            // filled in FULL, and the obvious reading ("an armed PartialFillNext
            // does not fire") was wrong - the arm had been evicted. The status
            // stays `202` because the requested arm WAS accepted; the body is
            // where the collateral damage is named.
            if let Some(shed) = state.engine.lock().await.arm(engine_div) {
                let body = format!(
                    "armed; the engine queue was at its {MAX_ARMED_DIVERGENCES}-entry cap, \
                     so the oldest armed divergence was discarded to make room: {shed:?}"
                );
                tracing::warn!(?shed, "arming ack reports an evicted divergence");
                return (StatusCode::ACCEPTED, body);
            }
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

/// HTTP order-entry surface for profiles that do not use `/ws` for orders.
///
/// The events come back in the response body as they always have. This path has
/// no pump, no socket and no backlog, so there is nothing for admission control
/// to protect: it gets lanes constructed FRESH PER REQUEST and dropped with it,
/// holding their own receivers so the forgotten permits are never stranded.
/// A refusal is therefore unreachable by construction - a per-request budget
/// starts empty of charges - and is mapped to a 500 with its reason rather than
/// silently unwrapped.
pub(crate) async fn submit_order_http(
    State(state): State<AppState>,
    Json(msg): Json<ClientMessage>,
) -> Result<Json<Vec<ServerMessage>>, StatusCode> {
    match msg {
        ClientMessage::Subscribe { .. } | ClientMessage::Unsubscribe { .. } => {
            Err(StatusCode::BAD_REQUEST)
        }
        order_cmd => {
            let (lanes, _receivers) = ExecLanes::detached();
            match process_order_cmd(order_cmd, &state, &lanes).await {
                OrderOutcome::Produced { events, .. } | OrderOutcome::Refused { events, .. } => {
                    Ok(Json(events))
                }
                OrderOutcome::Diagnostic(frame) => Ok(Json(vec![frame])),
                OrderOutcome::NotAdmitted(frame) => {
                    tracing::error!(
                        ?frame,
                        "per-request lanes refused admission; this should be unreachable"
                    );
                    Err(StatusCode::INTERNAL_SERVER_ERROR)
                }
            }
        }
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
