// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! HTTP surface: shared app state plus every plain request/response route
//! (`/instruments`, `/account`, `/clock`, `/trades`, `/quotes`,
//! `/control/divergence`). The stateful, streaming websocket surface
//! (`/ws`) lives in `ws.rs`; both share `AppState` and the order-entry
//! validation gate (`process_order_cmd`) defined here.

use std::sync::{Arc, atomic::Ordering};

use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use mogwai_data::TickEvent;
use mogwai_engine::MAX_ARMED_DIVERGENCES;
use mogwai_protocol::{
    AccountState, AdmissionSubject, ClientMessage, CommandClass, DEFAULT_HISTORY_LIMIT,
    InstrumentDef, MAX_HISTORY_LIMIT, OrderType, QuoteTick, ServerClock, ServerMessage, SimClock,
    TradeTick, control::Divergence, trades_through, truncate_client_id, truncate_reason,
    validate_client_order_id, validate_divergence, validate_modify_order, validate_request_id,
    validate_submit_order,
};
use serde::{Deserialize, Serialize};

use crate::admission::{ExecLanes, Reservation};
use crate::config::{Config, now_ns, sim_duration_from_millis, sim_now_ns};
use crate::run::Run;
use crate::source;

#[derive(Deserialize)]
pub(crate) struct DivergenceRequest {
    #[serde(default)]
    symbol: Option<String>,
    #[serde(flatten)]
    divergence: Divergence,
}

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
pub(crate) fn admission_subject(cmd: &ClientMessage) -> AdmissionSubject {
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
    }
}

/// The protocol-boundary verdict on one order-entry command: `Some(reason)`
/// when it is malformed. Ids are length-checked here, alongside the numeric
/// checks that were always here, because both are malformed-request failures.
pub(crate) fn boundary_error(cmd: &ClientMessage) -> Option<&'static str> {
    match cmd {
        ClientMessage::SubmitOrder(order) => validate_submit_order(order).err(),
        ClientMessage::ModifyOrder {
            client_order_id,
            price,
            quantity,
            trigger_price,
        } => validate_client_order_id(client_order_id)
            .err()
            .or_else(|| validate_modify_order(*price, *quantity, *trigger_price).err()),
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
/// The websocket order carrier uses this one gate for every command. `None`
/// means the command cleared the protocol boundary.
fn boundary_outcome(order_cmd: &ClientMessage, lanes: &ExecLanes, ts: u64) -> Option<OrderOutcome> {
    let reason = boundary_error(order_cmd)?;
    let Some(reservation) = lanes.try_reserve_boundary() else {
        return Some(OrderOutcome::NotAdmitted(
            ServerMessage::AdmissionRejected {
                subject: admission_subject(order_cmd),
                reason: "execution output admission budget exhausted".into(),
                ts_event: ts,
            },
        ));
    };
    let event = boundary_refusal(order_cmd, reason, ts);
    if matches!(event, ServerMessage::ProtocolError { .. }) {
        // The reservation is not needed after all: nothing goes on the
        // held lane, so drop it and give the bytes straight back.
        drop(reservation);
        return Some(OrderOutcome::Diagnostic(event));
    }
    Some(OrderOutcome::Refused {
        events: vec![event],
        reservation,
    })
}

pub(crate) async fn process_order_cmd(
    order_cmd: ClientMessage,
    state: &AppState,
    run: &Arc<Run>,
    lanes: &ExecLanes,
    socket_symbol: &mogwai_protocol::Symbol,
    boat: &Arc<crate::boatyard::Boat>,
) -> OrderOutcome {
    let sim = boat.sim;
    // Sampled at entry for the boundary rejections below: they return before
    // any price synthesis, so entry-time is when they logically occur.
    let ts = sim_now_ns(sim);
    if let Some(outcome) = boundary_outcome(&order_cmd, lanes, ts) {
        return outcome;
    }
    // A submit names its own symbol on the wire; it is CHECKED against the
    // river this socket bound, never overridden by it. Without the check a
    // client could bind one river and trade another.
    //
    // PLACEMENT IS THE CONTRACT: right after the protocol boundary and BEFORE
    // the act delay, the market reading, the calendar lookup and the engine
    // lock. A check placed lower would let a mismatched MARKET order drive
    // price synthesis, checkpoint-mutex and cache work for a river the socket
    // is not bound to before being refused. Refusing here also dates the
    // refusal at the entry-time `ts`, like its boundary neighbours.
    //
    // The frame is `OrderRejected`, NOT `AdmissionRejected`: a mismatch is a
    // client error, and `AdmissionRejected` reads as a capacity signal at every
    // observer. Only the failure to reserve a boundary slot is capacity.
    if let ClientMessage::SubmitOrder(order) = &order_cmd
        && order.symbol.as_ref() != socket_symbol.as_ref()
    {
        let Some(reservation) = lanes.try_reserve_boundary() else {
            return OrderOutcome::NotAdmitted(ServerMessage::AdmissionRejected {
                subject: admission_subject(&order_cmd),
                reason: "execution output admission budget exhausted".into(),
                ts_event: ts,
            });
        };
        let echoed: String = order.symbol.chars().take(MAX_ECHOED_SYMBOL).collect();
        return OrderOutcome::Refused {
            events: vec![ServerMessage::OrderRejected {
                client_order_id: order.client_order_id.clone(),
                reason: truncate_reason(format!(
                    "order symbol {echoed} does not match the symbol this connection is bound to ({socket_symbol})"
                )),
                ts_event: ts,
            }],
            reservation,
        };
    }
    // Register the symbol lazily, BEFORE the act delay and any market reading.
    // A `false` here means no profile resolves the symbol; fall through rather
    // than short-circuit, so an unconfigured symbol still meets the engine's
    // existing unknown-instrument rejection with its existing wording instead of
    // a second one invented here.
    if let ClientMessage::SubmitOrder(order) = &order_cmd {
        let _configured = run.ensure_instrument(&order.symbol).await;
    }
    // The venue's ACT delay sits BETWEEN the protocol boundary and the market
    // price stamp. After the boundary, because a malformed command is refused
    // by the protocol and a refusal is not a venue act - and the same reason
    // puts it after the bound-symbol check above. Before the stamp, because a
    // delayed submit must meet the tape as it is when the venue ACTS, not as it
    // was when the command arrived - and because the `ts` re-sample below then
    // dates the engine's events at act time with no second re-sample needed.
    //
    // It LIVES HERE, not in the ws dispatcher that used to hold it, because
    // only here is "after the boundary" expressible; the dispatcher ran it
    // before `boundary_outcome`, which contradicted this paragraph and delayed
    // refusals that are not acts.
    let class = CommandClass::of(&order_cmd);
    let act_ms = class.map_or(0, |class| run.act_ms(class));
    if act_ms > 0 {
        tokio::time::sleep(sim.wall_duration(sim_duration_from_millis(act_ms))).await;
    }
    // Re-sampled BEFORE the reading, not after the act sleep only in name: the
    // reading is "the last print at or before the venue ACTED", so
    // handing it the entry-time `ts` would judge a delayed submit against the
    // tape as it was when the command arrived - the exact staleness the act
    // delay is placed above the reading to avoid.
    let ts = sim_now_ns(sim);
    let (order_cmd, market_px) = market_reading(order_cmd, state, boat, ts, socket_symbol).await;
    // Re-sample after the market-price synthesis, which for a price-less MARKET
    // order may block ~100 ms on the checkpoint mutex and seek (S16). The
    // synthesis-failure reject and the engine events below all occur now.
    let ts = sim_now_ns(sim);
    // A MARKET order still price-less after the stamp, for a symbol this venue
    // DOES list, means market price synthesis failed (most likely the task
    // itself died).
    // Reject it here with the honest story - the client correctly sent no price
    // (nautilus never stamps a market order), so letting the engine's "submit
    // price required" fire would blame the client for the venue's own synthesis
    // failure. A symbol the resolver REFUSES - illegal, funding-barred, or one
    // whose resolved shape does not validate - is deliberately left price-less:
    // the engine checks instrument existence before the price, so its "unknown
    // instrument" rejection tells that story unaltered. Resolution is otherwise
    // total, so "does the venue list it" is now "did the resolve succeed".
    if let ClientMessage::SubmitOrder(order) = &order_cmd
        && order.order_type == OrderType::Market
        && order.price.is_none()
        && state.rivers.resolve_profile(&order.symbol).is_ok()
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
    if let ClientMessage::SubmitOrder(order) = &order_cmd
        // The RESOLVED shape's calendar, not the configured map's: a symbol
        // nobody configured is served on a resolved bundle whose tape is
        // generated with that calendar, so its orders owe the same
        // session-closed refusal.
        && let Some(calendar) = state
            .rivers
            .resolve_profile(&order.symbol)
            .ok()
            .and_then(|profile| profile.calendar.clone())
        && reject_while_closed(&calendar, ts, order, market_px)
    {
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
                reason: "market closed".into(),
                ts_event: ts,
            }],
            reservation,
        };
    }
    let mut engine = run.engine.lock().await;
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
    let events = engine.process_with_market_on_clock(order_cmd, ts, market_px, sim);
    drop(engine);
    OrderOutcome::Produced {
        events,
        reservation,
    }
}

fn reject_while_closed(
    calendar: &mogwai_data::SessionCalendar,
    ts: u64,
    order: &mogwai_protocol::SubmitOrder,
    market_px: Option<mogwai_engine::MarketReading>,
) -> bool {
    !calendar.is_open(ts)
        && (order.order_type == OrderType::Market
            || (order.order_type == OrderType::Limit
                && order.price.is_some_and(|price| {
                    market_px
                        .is_some_and(|reading| trades_through(order.side, price, reading.last_px))
                })))
}

/// The router's share of the run. Deliberately thin: the clock, the history
/// floor and the instrument all live on `Run`, which owns them, and are read
/// through it rather than copied here - a second copy of the clock is a second
/// thing that can be re-anchored out from under the tape it dates.
#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) run: Arc<Run>,
    pub(crate) cfg: Config,
    pub(crate) rivers: Arc<source::Rivers>,
    /// Process-wide ceiling on websocket commands sleeping out an armed ACT
    /// delay. The per-connection lane bounds one client; this bounds the run.
    pub(crate) pending_commands: Arc<tokio::sync::Semaphore>,
    pub(crate) history_requests: Arc<tokio::sync::Semaphore>,
}

#[derive(Serialize)]
pub(crate) struct Health {
    status: &'static str,
    oms_type: mogwai_protocol::OmsType,
    /// Identifies this RUN, not this process.
    ///
    /// The endpoint is an ephemeral port, and a port outlives nothing: once a
    /// venue exits, the number is free and anything may take it. A client
    /// holding that address has no way to tell the run it was launched against
    /// from whatever now answers there, and the window is not hypothetical -
    /// this venue stops accepting BEFORE it exits, draining live connections for
    /// up to the shutdown grace, so the port is free while the process is still
    /// alive and any consumer watching for child exit sees nothing.
    ///
    /// The seed is already unique per run and already reported in the readiness
    /// record, so a launcher can hand it to its clients and they can check they
    /// are still talking to the venue they were given.
    run_seed: u64,
    fault: Option<HealthFault>,
}

#[derive(Serialize)]
struct HealthFault {
    kind: &'static str,
    clock_ns: u64,
}

pub(crate) async fn health(State(state): State<AppState>) -> Json<Health> {
    Json(Health {
        status: "ok",
        oms_type: state.run.oms_type,
        run_seed: state.run.seeds.run,
        fault: state
            .run
            .boatyard
            .boat_for_symbol(&state.run.boot_symbol)
            .and_then(|boat| boat.tape.fault())
            .map(|fault| match fault {
                mogwai_data::TickFault::Arrival(mogwai_data::ArrivalRefusal::NoOpenExposure {
                    from_ns,
                }) => HealthFault {
                    kind: "arrival.no_open_exposure",
                    clock_ns: from_ns,
                },
                mogwai_data::TickFault::Arrival(
                    mogwai_data::ArrivalRefusal::IntensityCeiling { clock_ns, .. },
                ) => HealthFault {
                    kind: "arrival.intensity_ceiling",
                    clock_ns,
                },
                mogwai_data::TickFault::Arrival(mogwai_data::ArrivalRefusal::NonFiniteState {
                    clock_ns,
                }) => HealthFault {
                    kind: "arrival.non_finite_state",
                    clock_ns,
                },
            }),
    })
}

impl AppState {
    /// The venue's one wall-to-sim reference. The only callers left are the
    /// answers no boat can give: a boatless river, the venue deadline, and the
    /// venue-scoped account ledger. Anything about a symbol calls `river_now`.
    pub(crate) fn venue_sim(&self) -> SimClock {
        self.run.sim
    }

    /// How far a request about `symbol` may be answered, and on whose clock.
    ///
    /// Async because it AWAITS an in-flight placement rather than falling
    /// through to the venue clock. A request racing a boarding would otherwise
    /// receive a well-formed answer off a clock strictly ahead of the boat that
    /// is about to be seated - invisible precisely because it is well-formed.
    pub(crate) async fn river_now(&self, symbol: &str) -> RiverNow {
        if let Some(boat) = self
            .run
            .boatyard
            .boat_for_symbol_awaiting_placement(symbol)
            .await
        {
            RiverNow {
                ns: boat.published_ns.load(Ordering::Acquire),
                sim: boat.sim,
                from_boat: true,
            }
        } else {
            RiverNow::venue(self.venue_sim())
        }
    }
}

/// How a river's now was resolved, and on what clock.
///
/// A boated river answers with what its boat has PUBLISHED; a boatless river
/// answers with the venue clock, which is the only ceiling water nobody is
/// carrying has. Never the boat's affine projection: a boat is deliberately
/// behind its own map, and a ceiling above the published tape is a look-ahead.
pub(crate) struct RiverNow {
    /// The ceiling a request about this symbol may be answered as of.
    pub(crate) ns: u64,
    /// The clock that instant lives on - the boat's when boated, the venue's
    /// otherwise. `/clock` renders this whole value.
    pub(crate) sim: SimClock,
    /// True when `sim` is a boat's. Renders as `ServerClock::boat_clock`.
    pub(crate) from_boat: bool,
}

impl RiverNow {
    /// The venue's own answer, for the questions that name no river at all.
    pub(crate) fn venue(sim: SimClock) -> Self {
        Self {
            ns: sim_now_ns(sim),
            sim,
            from_boat: false,
        }
    }
}

/// Control plane: arm a divergence to fire on its next trigger. It is armed
/// against the RUN, so it reaches every open connection: there is no account to
/// divert it onto.
pub(crate) async fn arm_divergence(
    State(state): State<AppState>,
    Json(request): Json<DivergenceRequest>,
) -> impl IntoResponse {
    let div = request.divergence;
    // Reject an invalid control payload before anything is stored. A typo must
    // be a no-op.
    if let Err(err) = validate_divergence(&div) {
        tracing::warn!(?div, err, "rejecting out-of-range divergence");
        return (StatusCode::BAD_REQUEST, err.to_string());
    }
    let run = &state.run;
    tracing::info!(?div, "arming divergence");
    // Validate at the arming boundary so an out-of-range knob (e.g. a
    // `PartialFillNext.fraction` outside `(0, 1]`) is rejected before it is
    // stored into server state or armed on the engine, rather than surfacing
    // as a degenerate fill downstream.
    match div {
        Divergence::DelayAcks { ms } => {
            run.delay_ms.store(ms, Ordering::Relaxed);
        }
        // STORE-not-merge, like every other server-owned window: one arm
        // REPLACES all six values, so an omitted field is armed as zero rather
        // than left standing from an earlier arm.
        Divergence::CommandLatency {
            submit_act_ms,
            modify_act_ms,
            cancel_act_ms,
            submit_ack_ms,
            modify_ack_ms,
            cancel_ack_ms,
        } => {
            run.submit_act_ms.store(submit_act_ms, Ordering::Relaxed);
            run.modify_act_ms.store(modify_act_ms, Ordering::Relaxed);
            run.cancel_act_ms.store(cancel_act_ms, Ordering::Relaxed);
            run.submit_ack_ms.store(submit_ack_ms, Ordering::Relaxed);
            run.modify_ack_ms.store(modify_ack_ms, Ordering::Relaxed);
            run.cancel_ack_ms.store(cancel_ack_ms, Ordering::Relaxed);
        }
        // GoDark/StallData windows are STORE-not-extend (S18): each arm replaces
        // the whole armed span under one lock, so re-arming with a SMALLER `ms`
        // shortens an in-flight blackout rather than lengthening it. This is
        // deliberate - re-arming sets the window, it does not accumulate - and lets
        // a test cut a window short by re-posting a small one; an operator wanting a
        // longer window re-arms with the longer `ms`.
        //
        // The window is stored CLOCK-NEUTRALLY, as a wall arming instant plus a
        // simulated span, because the armer cannot know who will read it: a
        // passenger may board afterwards, and every reader judges the span on
        // its own boat clock.
        Divergence::GoDark { ms } => {
            run.dark.arm(now_ns(), sim_duration_from_millis(ms));
        }
        Divergence::StallData { ms } => {
            run.stall.arm(now_ns(), sim_duration_from_millis(ms));
        }
        Divergence::FlowSurge {
            rate_mult,
            children_mult,
            duration_ms,
        } => {
            let symbol = if let Some(symbol) = request.symbol.as_deref() {
                symbol
            } else {
                let seated = run.boatyard.seated_symbols();
                if !seated.is_empty() {
                    return (
                        StatusCode::BAD_REQUEST,
                        format!(
                            "generator divergence requires symbol; seated boats: {}",
                            seated.join(", ")
                        ),
                    );
                }
                run.boot_symbol.as_ref()
            };
            if run.boatyard.boat_for_symbol(symbol).is_some() {
                return (
                    StatusCode::BAD_REQUEST,
                    format!(
                        "river {symbol} has a seated boat; place a boat whose sharing key carries generator havoc"
                    ),
                );
            }
            // The late-boarder rule, collapsed to its only reachable case. This
            // arm is refused above unless the river is BOATLESS, and every boat
            // placed on a river anchors `sim_epoch_ns` at the yard's origin,
            // which is `run.started_ns`. So the instant a future boat would open
            // this window at IS the run origin, and stamping it here is the same
            // answer `HavocWindow::open_at` computes for the transport windows.
            // Stamping `sim_now_ns(venue_sim)` instead - what this used to do -
            // put the window in that boat's far future, where it could never be
            // entered at all.
            if !state.rivers.arm_flow_surge(
                symbol,
                run.started_ns,
                duration_ms,
                rate_mult,
                children_mult,
            ) {
                return (
                    StatusCode::BAD_REQUEST,
                    format!("symbol {symbol} has no configured shape"),
                );
            }
            // Named in the ack so an operator can see WHICH span was armed and
            // against which origin, rather than inferring it from a bare 202.
            return (
                StatusCode::ACCEPTED,
                format!(
                    "armed a {duration_ms} ms simulated surge on {symbol}, opening at the river origin {origin}",
                    origin = run.started_ns
                ),
            );
        }
        Divergence::FeeSurcharge { mult, window_ms } => {
            run.engine.lock().await.arm_fee_surcharge(
                mult,
                now_ns(),
                sim_duration_from_millis(window_ms),
            );
        }
        // Immediate book action, not an armed trigger: cancel the resting
        // order right now, silently (no lifecycle event - that lost event IS
        // the injected fault; the truth surfaces only via QueryOrders). A
        // miss - unknown id, or already terminal - is refused with a 404 so
        // a scenario cannot believe it armed a fault that never happened.
        Divergence::CancelOpenOrderSilently { client_order_id } => {
            // The clock comes from the TARGETED ORDER, never from a request
            // field: `client_order_id` already determines the order, hence its
            // symbol and its river, and a request-supplied `symbol` can disagree
            // with it. A mismatch is refused rather than silently preferred
            // either way.
            let Some(order_symbol) = run.engine.lock().await.open_order_symbol(&client_order_id)
            else {
                // No resting order carries this id. Let the engine say WHY -
                // unknown id and already-terminal are different diagnoses, and
                // this arm must not flatten them into one message. The clock
                // handed in is irrelevant: this call can only fail.
                let reason = run
                    .engine
                    .lock()
                    .await
                    .cancel_open_order_silently(&client_order_id, sim_now_ns(state.venue_sim()))
                    .err()
                    .unwrap_or_else(|| "unknown order".to_owned());
                tracing::warn!(%client_order_id, reason, "refusing silent cancel");
                return (StatusCode::NOT_FOUND, reason);
            };
            if let Some(request_symbol) = request.symbol.as_deref()
                && request_symbol != order_symbol.as_ref()
            {
                return (
                    StatusCode::BAD_REQUEST,
                    format!(
                        "silent cancel names symbol {request_symbol}, but order {client_order_id} rests on {order_symbol}"
                    ),
                );
            }
            let ts = state.river_now(&order_symbol).await.ns;
            if let Err(reason) = run
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
            // Lift both server-owned temporal windows. `None` is the cleared
            // state and is closed on every reader clock. There is no backlog
            // to replay because gated frames are dropped.
            // The generator half is decided BEFORE anything is lifted: a
            // refused control must be a no-op, and returning here after the
            // stores below would leave the transport windows cleared under a
            // `400` that says nothing happened.
            let clearing: Vec<String> = match request.symbol.as_deref() {
                Some(symbol) => {
                    if run.boatyard.boat_for_symbol(symbol).is_some() {
                        return (
                            StatusCode::BAD_REQUEST,
                            format!(
                                "river {symbol} has a seated boat; generator controls cannot mutate live water"
                            ),
                        );
                    }
                    vec![symbol.to_owned()]
                }
                // Unqualified, this clears every river whose water exists and
                // carries no boat. A seated river is SKIPPED rather than
                // refused: the transport half of this control is run-wide and
                // must stay reachable while a boat is sitting.
                None => state
                    .rivers
                    .materialized_symbols()
                    .into_iter()
                    .filter(|symbol| run.boatyard.boat_for_symbol(symbol).is_none())
                    .collect(),
            };
            run.delay_ms.store(0, Ordering::Relaxed);
            run.dark.clear();
            run.stall.clear();
            for symbol in clearing {
                state.rivers.clear_flow_surge(&symbol);
            }
            run.engine.lock().await.clear_fee_surcharge();
            // All six `CommandLatency` fields go with them. This clears what the
            // venue will do to commands it has NOT started acting on yet, and it
            // lifts an ack window off frames already queued (the pump reads that
            // one per event at dequeue). It does NOT reach into an act delay
            // already being served: that command's sleep was read once, at
            // detach, and a venue that has begun acting does not un-begin.
            run.submit_act_ms.store(0, Ordering::Relaxed);
            run.modify_act_ms.store(0, Ordering::Relaxed);
            run.cancel_act_ms.store(0, Ordering::Relaxed);
            run.submit_ack_ms.store(0, Ordering::Relaxed);
            run.modify_ack_ms.store(0, Ordering::Relaxed);
            run.cancel_ack_ms.store(0, Ordering::Relaxed);
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
                        | Divergence::CommandLatency { .. }
                        | Divergence::GoDark { .. }
                        | Divergence::StallData { .. }
                        | Divergence::FlowSurge { .. }
                        | Divergence::FeeSurcharge { .. }
                        | Divergence::ClearDivergences
                        | Divergence::CancelOpenOrderSilently { .. }
                ),
                "server-owned divergences must not be forwarded to engine.arm()",
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
            if let Some(shed) = run.engine.lock().await.arm(engine_div) {
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

/// Every shape this run configured UNION every symbol it has since
/// materialized, sorted by symbol, each of them servable for history.
///
/// Websocket upgrades place paced boats on demand for any served symbol, and a
/// history poll materializes a river too - so this set grows during a run.
pub(crate) async fn instruments(State(state): State<AppState>) -> Json<Vec<InstrumentDef>> {
    Json(state.rivers.instrument_defs())
}

/// The HTTP shape of a PULLED account snapshot.
///
/// `AccountState` itself is UNCHANGED - it is also the pushed frame's payload,
/// and the pushed path is per-boat and already correct. The label is added by
/// this response only, and `serde(flatten)` keeps every existing field at the
/// same position in the object, so a consumer that ignores unknown fields
/// (`mogwai-adapter`'s `client/shared.rs` among them) parses it unchanged.
#[derive(Serialize)]
pub(crate) struct AccountSnapshot {
    /// Always `"venue"` today. Present so a consumer can never mistake the
    /// `ts_event` here for boat time.
    clock: ClockAxis,
    #[serde(flatten)]
    account: AccountState,
}

/// Which axis a timestamp lives on. The sibling of `ServerClock::boat_clock`,
/// and the reason a venue stamp is honest rather than a look-ahead in disguise.
#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum ClockAxis {
    Venue,
}

/// Pull route for the venue's current account snapshot.
///
/// `AccountState` is execution-owned and is otherwise only pushed with an order
/// event. An adapter pulls this once on connect so the bridge's account row
/// exists before the first order is worked, rather than learning the account
/// only when the first fill's `AccountState` arrives.
///
/// STAMPED ON THE VENUE CLOCK, deliberately. The ledger is venue-scoped - one
/// engine serving every river - so there is no boat axis to put it on: stamp
/// from the boot boat and a push from a later-placed boat on another river is
/// AHEAD of the pull; stamp from the newest boat and it is behind. No choice
/// can keep a cross-clock monotonicity promise, so the answer keeps the venue
/// stamp and says so, and a consumer orders pulls against pushes BY SEQUENCE.
pub(crate) async fn account(State(state): State<AppState>) -> Json<AccountSnapshot> {
    let ts = sim_now_ns(state.venue_sim());
    let mut engine = state.run.engine.lock().await;
    Json(AccountSnapshot {
        clock: ClockAxis::Venue,
        account: engine.account_snapshot(ts),
    })
}

#[derive(Default, Deserialize)]
pub(crate) struct ClockQuery {
    #[serde(default)]
    symbol: Option<String>,
}

pub(crate) async fn clock(
    Query(query): Query<ClockQuery>,
    State(state): State<AppState>,
) -> Json<ServerClock> {
    // Publish the tape boundary alongside the affine map so a client can guard
    // its own warmup against `data_origin_ns` rather than issuing a doomed
    // off-tape fetch. `server_now_ns` is sampled here so the client gets sim-now
    // and the floor from one round trip, without reading its own (skewable) wall
    // clock. `data_origin_ns` is the fixed floor; the horizon is echoed so
    // the client can report the floor in its own terms.
    //
    // `?symbol=` answers for that river's boat, of which there is at most one.
    // For a boat, `server_now_ns` is the sim instant of the last tick it
    // PUBLISHED rather than the affine map evaluated at the wall: a boat placed
    // mid-run is deliberately behind its own clock's projection, and a client
    // pacing against a projection the feed has not reached would ask for water
    // that has not been delivered. With no boat the two coincide.
    let now = if let Some(symbol) = query.symbol.as_deref() {
        state.river_now(symbol).await
    } else {
        RiverNow::venue(state.venue_sim())
    };
    Json(ServerClock {
        sim: now.sim,
        server_now_ns: now.ns,
        data_origin_ns: state.run.data_origin_ns(),
        warmup_ns: state.run.warmup_ns,
        boat_clock: now.from_boat,
    })
}

/// The venue's tape reading for one command, taken on the way in.
///
/// EVERY submit takes one, and so does every PRICE amend: a limit needs it to
/// size the band its trigger is drawn from and to judge marketability, a market
/// order needs it to price its slippage, and an amend needs it so the re-draw
/// adopts the current regime instead of the acceptance one. A quantity-only
/// amend and every non-order message take none.
///
/// A wire MARKET order carries no price (mirroring Nautilus, which never stamps
/// one), but the engine has no book of its own, so a price-less market order is
/// additionally STAMPED here with the last print at or before `ts`. That is the
/// same number the reading carries; the former separate price path returned
/// the first tick at or AFTER sim-now, which is a
/// look-ahead, and keeping two sources of "what is the market" is how they drift
/// apart. `fills::read_last` is consulted only when `read_market` refuses
/// outright, since the protocol still requires a price on the wire.
///
/// The reading is otherwise RETURNED, never stamped - an order keeps its own
/// stated price, and what the venue read is the engine's business.
///
/// Synthesis (`build_history_source` -> `source_at_or_before`) locks the run's
/// checkpoint mutex and walks the residual past the nearest checkpoint.
/// `/trades` pushes the identical synthesis onto `spawn_blocking` rather than
/// running it inline on the tokio worker; this does the same, so a burst of price-less market
/// orders cannot stall the runtime's worker pool or serialize behind a seeked
/// `/trades` request (or vice versa) any longer than the symbol's own river
/// lock requires. Since the registry keys one lock per river, contention is
/// per symbol by construction: another symbol's requests take another
/// river's lock and do not queue here at all (S13).
async fn market_reading(
    msg: ClientMessage,
    state: &AppState,
    boat: &Arc<crate::boatyard::Boat>,
    ts: u64,
    socket_symbol: &mogwai_protocol::Symbol,
) -> (ClientMessage, Option<mogwai_engine::MarketReading>) {
    // Whether this command needs a reading at all. WHICH river it is read for
    // is never in question: a submit's wire symbol was already refused unless
    // it equals the socket's, an amend names no symbol at all, and the socket's
    // symbol is the one its boat was boarded on. Both the memoized walk and the
    // `read_last` fallback below therefore take the boat's own river, so the
    // two cannot name different rivers even if that resolution ever changes.
    let needs_reading = match &msg {
        ClientMessage::SubmitOrder(order) => {
            debug_assert_eq!(order.symbol.as_ref(), boat.symbol());
            true
        }
        // An amend names no symbol on the wire, so it resolves to the river
        // this socket is bound to.
        ClientMessage::ModifyOrder { price: Some(_), .. } => {
            debug_assert_eq!(socket_symbol.as_ref(), boat.symbol());
            true
        }
        _ => false,
    };
    if needs_reading {
        let rivers = Arc::clone(&state.rivers);
        let mult = state.cfg.fill_band_vol_mult;
        let max_ticks = state.cfg.fill_band_max_ticks;
        let interval_ms = state.cfg.fill_sweep_interval_ms;
        let boat = Arc::clone(boat);
        let (reading, last_px) = tokio::task::spawn_blocking(move || {
            let reading = boat
                .market_readings
                .read(ts, &rivers, mult, max_ticks, interval_ms);
            let last_px = reading
                .map(|value| value.last_px)
                .or_else(|| crate::fills::read_last(boat.symbol(), ts, &rivers));
            (reading, last_px)
        })
        .await
        .unwrap_or_else(|e| {
            // A failed reading is simply no reading: the order rests and the
            // sweeper decides it. Never a fill the venue could not price.
            tracing::error!(%e, "market reading task failed");
            (None, None)
        });
        let msg = match msg {
            ClientMessage::SubmitOrder(mut order)
                if order.order_type == OrderType::Market && order.price.is_none() =>
            {
                order.price = last_px;
                ClientMessage::SubmitOrder(order)
            }
            other => other,
        };
        return (msg, reading);
    }
    (msg, None)
}

#[derive(Debug, Deserialize)]
pub(crate) struct HistoryQuery {
    pub(crate) symbol: String,
    pub(crate) start: Option<u64>,
    pub(crate) end: Option<u64>,
    pub(crate) limit: Option<usize>,
}

pub(crate) async fn trades(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<HistoryQuery>,
) -> Result<axum::response::Response, (StatusCode, String)> {
    let history_slot = admit_history(&state.history_requests)?;
    // No `regime` parameter: the market regime is boot config for the whole run
    // now, so history and the live tape are the same realization by
    // construction rather than by a client remembering to ask for the same one.
    let limit = normalize_limit(query.limit);
    let rivers = Arc::clone(&state.rivers);
    let data_origin = source::TAPE_ORIGIN_NS;
    let HistoryQuery {
        symbol, start, end, ..
    } = query;
    // Resolution is total, so the only history refusals left are shape-class
    // ones - an illegal label, an invalid or funding-barred shape, an exhausted
    // river cap - and they are decided HERE so each is a 400 naming its reason
    // rather than a 500 raised out of the synthesis task below. Materializing
    // here also means the poll advertises through `/instruments`, which is the
    // same event as spending the river.
    if let Err(error) = rivers.materialize(&symbol) {
        return Err((StatusCode::BAD_REQUEST, error.to_string()));
    }
    // The ceiling is the NAMED RIVER's now, not the venue's.
    let river_now = state.river_now(&symbol).await.ns;
    if let Some(body) = history_start_refusal(start, data_origin, river_now) {
        return Err((StatusCode::BAD_REQUEST, body));
    }

    // An explicit `end` past the ceiling is CLAMPED rather than refused, and
    // that asymmetry with the `start` refusal above is deliberate. A start past
    // the ceiling asks for a window that lies entirely beyond what this river
    // has produced and can only be a caller error; an end past it is the
    // ordinary "give me everything up to now" request, which a consumer writes
    // by stamping a clock of its own.
    //
    // That gap is NOT a hair. The ceiling here is the river's own now - what
    // its boat has PUBLISHED - and a boat placed `T` wall-nanoseconds after
    // boot sits `T * speed` simulated nanoseconds behind the venue clock,
    // permanently and by construction. So a client that reads `/clock` with no
    // `?symbol=` and passes that instant as `end` is routinely far ahead of
    // this answer, and refusing it would fail every honest warmup fetch to
    // prevent nothing: the clamp serves exactly the data that exists and no
    // more, so nothing is silently short about it. A client that needs to know
    // where the tail actually is reads `/clock?symbol=`.
    //
    // Clamping the served tail also stops a no-`end` request - which otherwise
    // grinds out the full `limit` however far past the ceiling that lands -
    // from streaming ticks the river has not published. The bound is
    // inclusive, so a tick landing exactly at the ceiling is still served.
    let end = Some(end.map_or(river_now, |end| end.min(river_now)));

    // Synthesizing up to `MAX_HISTORY_LIMIT` ticks is pure CPU work against the
    // generator (the source never blocks on IO), and `next_tick` never returns
    // `None`, so a request grinds until it fills `limit` or crosses the
    // effective `end`. Run it on a blocking thread rather than inline on the
    // tokio worker, so a burst of `/trades` requests cannot stall the async
    // runtime's worker pool.
    //
    // Serialization runs on the SAME blocking task as the synthesis, and the
    // admission permit RIDES that task rather than staying a handler local:
    // hyper drops this handler future the moment the client disconnects, but a
    // running blocking task cannot be cancelled, so a permit left out here
    // would be released while the synthesis it admitted was still resident -
    // the exact scope-ends-before-the-work defect `HistoryPage` closes on the
    // response side. The closure hands the slot back with the bytes; on the
    // error and panic paths it drops inside the task, after the work ends.
    // Echoed back on the failure paths below, since the handler's own copy is
    // moved into the blocking task.
    let named = truncated_symbol(&symbol);
    let (history_slot, body) = match tokio::task::spawn_blocking(move || {
        let body = bounded_trades(&symbol, start, end, limit, &rivers)
            .and_then(|rows| serde_json::to_vec(&rows).map_err(Into::into));
        (history_slot, body)
    })
    .await
    {
        Ok((slot, Ok(body))) => (slot, body),
        // A synthesis failure is NOT an empty window. Naming the symbol and the
        // window here is what keeps a blown reach or a stopped generator
        // distinguishable from a span nothing traded in.
        Ok((_slot, Err(e))) => {
            tracing::error!(%e, symbol = %named, "trade history could not be produced");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                history_failure_body(&named, start, end),
            ));
        }
        Err(e) => {
            tracing::error!(%e, symbol = %named, "history synthesis task failed");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                history_failure_body(&named, start, end),
            ));
        }
    };
    Ok(history_page(history_slot, body))
}

pub(crate) async fn quotes(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<HistoryQuery>,
) -> Result<axum::response::Response, (StatusCode, String)> {
    let history_slot = admit_history(&state.history_requests)?;
    let limit = normalize_limit(query.limit);
    let rivers = Arc::clone(&state.rivers);
    let data_origin = source::TAPE_ORIGIN_NS;
    let HistoryQuery {
        symbol, start, end, ..
    } = query;
    // Resolution is total, so the only history refusals left are shape-class
    // ones - an illegal label, an invalid or funding-barred shape, an exhausted
    // river cap - and they are decided HERE so each is a 400 naming its reason
    // rather than a 500 raised out of the synthesis task below. Materializing
    // here also means the poll advertises through `/instruments`, which is the
    // same event as spending the river.
    if let Err(error) = rivers.materialize(&symbol) {
        return Err((StatusCode::BAD_REQUEST, error.to_string()));
    }
    // The ceiling is the NAMED RIVER's now, not the venue's.
    let river_now = state.river_now(&symbol).await.ns;
    if let Some(body) = history_start_refusal(start, data_origin, river_now) {
        return Err((StatusCode::BAD_REQUEST, body));
    }
    // A future start is impossible, while a future end is the ordinary
    // caller-clock spelling of "everything through now", so clamp the latter.
    let end = Some(end.map_or(river_now, |end| end.min(river_now)));
    // The permit rides the blocking task for the same reason as `/trades`: a
    // client disconnect drops this future, and only the closure outlives that.
    let named = truncated_symbol(&symbol);
    let (history_slot, body) = match tokio::task::spawn_blocking(move || {
        let body = bounded_quotes(&symbol, start, end, limit, &rivers)
            .and_then(|rows| serde_json::to_vec(&rows).map_err(Into::into));
        (history_slot, body)
    })
    .await
    {
        Ok((slot, Ok(body))) => (slot, body),
        Ok((_slot, Err(e))) => {
            tracing::error!(%e, symbol = %named, "quote history could not be produced");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                history_failure_body(&named, start, end),
            ));
        }
        Err(e) => {
            tracing::error!(%e, symbol = %named, "quote history synthesis task failed");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                history_failure_body(&named, start, end),
            ));
        }
    };
    Ok(history_page(history_slot, body))
}

/// How many `/trades` or `/quotes` syntheses may be in flight at once. A fifth
/// request is REFUSED rather than queued, so history can neither fill Tokio's
/// blocking pool ahead of order-entry market readings nor accumulate response
/// buffers without a ceiling.
///
/// The memory bound this buys is MEASURED rather than asserted, by
/// `worst_case_history_page_bytes` and recorded in `reference/performance.md`:
/// a full `/quotes` page is 4.40 MB of `QuoteTick` vector and 5.90 MB of JSON
/// resident together while it serializes, so four of them peak near 41 MB.
/// `/trades` is narrower at 3.20 MB plus 5.05 MB. The number is a bound only
/// because the permit outlives BOTH halves, which is what `HistoryPage` is
/// for.
pub(crate) const MAX_CONCURRENT_HISTORY_REQUESTS: usize = 4;

/// A serialized history page that owns its admission permit.
///
/// The permit cannot simply be a handler local. Axum serializes a returned
/// `Json` value AFTER the handler future resolves, so a permit dropped at the
/// end of the handler is released while multi-megabyte responses are still
/// being built - four completed syntheses would readmit four more while their
/// bytes were still resident, and the ceiling above would bound nothing. So
/// serialization happens on the synthesis's own blocking task and the permit
/// travels with the finished bytes: it is released when hyper drops this body,
/// which is after the response has been written.
///
/// The permit's OTHER end is guarded too: it is moved INTO the blocking
/// closure, not merely awaited past, because a client disconnect drops the
/// handler future while the blocking task keeps running to completion. A
/// permit held by the dropped future would readmit a new synthesis while the
/// orphaned one was still resident; one held by the closure returns only when
/// the work it admitted actually ends.
struct HistoryPage {
    _slot: tokio::sync::OwnedSemaphorePermit,
    body: Option<axum::body::Bytes>,
}

impl futures_util::Stream for HistoryPage {
    type Item = Result<axum::body::Bytes, std::convert::Infallible>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::task::Poll::Ready(self.get_mut().body.take().map(Ok))
    }
}

fn history_page(
    slot: tokio::sync::OwnedSemaphorePermit,
    body: Vec<u8>,
) -> axum::response::Response {
    let length = body.len();
    let mut response = axum::response::Response::new(axum::body::Body::from_stream(HistoryPage {
        _slot: slot,
        body: Some(axum::body::Bytes::from(body)),
    }));
    let headers = response.headers_mut();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    // A streamed body advertises no size, so without this hyper falls back to
    // chunked transfer encoding - which is legal HTTP and which the crate's own
    // raw-socket test client does not implement. The length is known exactly
    // here (the whole page is already serialized), so state it and keep the
    // response byte-shaped exactly as the `Json` it replaced.
    headers.insert(
        axum::http::header::CONTENT_LENGTH,
        axum::http::HeaderValue::from(length),
    );
    response
}

/// The one admission decision both history endpoints make, so the cap and its
/// refusal cannot drift apart between `/trades` and `/quotes`. Fail-fast by
/// construction: `try_acquire_owned` never waits, so an over-capacity request
/// is answered immediately rather than queued behind four syntheses.
fn admit_history(
    gate: &Arc<tokio::sync::Semaphore>,
) -> Result<tokio::sync::OwnedSemaphorePermit, (StatusCode, String)> {
    Arc::clone(gate).try_acquire_owned().map_err(|_| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "history request capacity exhausted".to_owned(),
        )
    })
}

/// A caller-supplied symbol cut to what any log line or body may echo.
fn truncated_symbol(symbol: &str) -> String {
    symbol.chars().take(MAX_ECHOED_SYMBOL).collect()
}

/// The 500 body for a history request the venue could not synthesize. Names the
/// symbol and the window so an operator can tell it from `200 []`.
fn history_failure_body(symbol: &str, start: Option<u64>, end: Option<u64>) -> String {
    let start = start.unwrap_or(source::TAPE_ORIGIN_NS);
    match end {
        Some(end) => format!("history for {symbol} over [{start}, {end}] could not be synthesized"),
        None => format!("history for {symbol} from {start} could not be synthesized"),
    }
}

/// How much of a caller-supplied symbol any refusal body, log line or reject
/// reason may echo.
///
/// Echo the request back TRUNCATED, for the reason `truncate_client_id` exists
/// on the order path: none of those may become an amplifier for an arbitrarily
/// long caller-supplied string. The cap is far above any symbol a run can
/// serve, so it only ever shortens a request that was going to be refused
/// anyway. Module-level rather than function-local because the `/ws` resolver
/// and the bound-symbol order refusal echo under the same rule.
const MAX_ECHOED_SYMBOL: usize = 64;

/// The symbol one `/ws` upgrade binds to, or the refusal body for a request
/// that is not a legal symbol at all.
///
/// WIRE LEGALITY IS THE ONLY GATE HERE. Resolution is total, so this function
/// no longer asks whether anybody configured the label; the shape refusals -
/// an invalid resolved shape, a funding-barred one, an exhausted river cap -
/// belong to `Run::ensure_instrument` and the boatyard, which is where the
/// resource is actually spent.
///
/// Takes `bound: &Symbol` and not `&Run` deliberately: it reads exactly
/// `run.boot_symbol`, and taking the whole run would make the unit tests
/// below impossible to write without spawning a tape.
///
/// The charset rule is stated on the DECODED value, which is the whole of it
/// server-side: `axum::extract::Query` percent-decodes before serde sees the
/// field, so `?symbol=%4DNQ` arrives as `MNQ` and passes, exactly as it should.
/// The needs-no-encoding framing belongs to the client-side caller of
/// `validate_wire_symbol`, which builds a URL by concatenation.
///
/// The comparison against `bound` is EXACT and case-sensitive, and so is
/// resolution: `mnq` is a DIFFERENT label from `MNQ` and therefore a different
/// river, even though `[symbols.*]` overlays match case-insensitively. That is
/// the symbol-as-a-label model applied consistently; a case-folding `/ws` would
/// bind a socket under one label whose history fetches name another.
pub(crate) fn resolve_socket_symbol(
    requested: Option<&str>,
    bound: &mogwai_protocol::Symbol,
) -> Result<mogwai_protocol::Symbol, String> {
    let Some(requested) = requested else {
        return Ok(Arc::clone(bound));
    };
    if mogwai_protocol::validate_wire_symbol(requested).is_err() {
        let echoed: String = requested.chars().take(MAX_ECHOED_SYMBOL).collect();
        return Err(format!(
            "requested symbol {echoed} is not a legal symbol; symbols are 1 to 32 characters of ASCII letters, digits, dot, dash or underscore"
        ));
    }
    Ok(Arc::from(requested))
}

/// Refuse a history start outside the tape that exists now.
///
/// A start before `data_origin` can never be served, so naming the floor keeps
/// that impossible request distinct from "no trades happened". With today's
/// zero origin that lower branch is unreachable for a `u64`, but it remains
/// tied to the constant the handlers read so moving the origin makes it live.
/// A start beyond `river_now` is likewise impossible without a look-ahead leak.
/// `river_now` is the NAMED RIVER's ceiling, not the venue's: for a seated
/// river it is what its boat published, and only a boatless river answers with
/// venue sim-now. An end beyond the ceiling is instead clamped at each
/// handler's call site: callers commonly mean "everything through now" and
/// stamp a clock of their own that leads this one.
fn history_start_refusal(start: Option<u64>, data_origin: u64, river_now: u64) -> Option<String> {
    let start = start?;
    if start < data_origin {
        tracing::warn!(start, data_origin, "refusing off-tape history window");
        return Some(format!(
            "requested start {start} precedes data_origin_ns {data_origin}; the tape cannot serve before its origin"
        ));
    }
    (start > river_now).then(|| {
        tracing::warn!(start, river_now, "refusing future history window");
        format!(
            "requested start {start} exceeds this river's now {river_now} - what its boat has published, or venue sim-now if it carries none; the tape cannot serve past the clock"
        )
    })
}

#[cfg(test)]
mod history_admission_tests {
    use super::*;

    /// Drives the endpoints' own admission decision rather than a semaphore of
    /// its own: `admit_history` is what both handlers call, so widening the cap
    /// or turning the refusal into a wait is visible here. Disclosed residual -
    /// this pins the decision, not that a handler still makes it; the handlers
    /// call it on their first line, and no in-crate test can build an
    /// `AppState` cheaply enough to prove the call site.
    #[test]
    fn history_concurrency_refuses_instead_of_queueing() {
        assert_eq!(MAX_CONCURRENT_HISTORY_REQUESTS, 4);
        let gate = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_HISTORY_REQUESTS));
        let held = (0..MAX_CONCURRENT_HISTORY_REQUESTS)
            .map(|_| admit_history(&gate).expect("slot"))
            .collect::<Vec<_>>();
        let (status, reason) = admit_history(&gate).expect_err("the fifth request is refused");
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(reason, "history request capacity exhausted");
        drop(held);
        assert!(admit_history(&gate).is_ok(), "a returned slot is reusable");
    }

    #[test]
    fn quote_history_refuses_below_a_nonzero_origin() {
        let refusal = history_start_refusal(Some(9), 10, 20).expect("off-tape refusal");
        assert!(refusal.contains("precedes data_origin_ns 10"));
        assert!(history_start_refusal(Some(10), 10, 20).is_none());
    }

    fn refusal_profiles(symbols: &[&str]) -> source::InstrumentProfiles {
        source::InstrumentProfiles::from_profiles(
            symbols
                .iter()
                .map(|symbol| {
                    crate::config::profile_for_symbol(symbol)
                        .unwrap_or_else(|error| panic!("{symbol} profile must resolve: {error}"))
                })
                .collect(),
        )
    }

    /// Re-anchored by piece 13: `configured_symbols` is no longer "what this
    /// run can serve" (resolution is total) but "what an operator named", and
    /// it is still sorted because refusal and diagnostic bodies read it.
    #[test]
    fn configured_symbols_are_sorted_and_case_exact() {
        let profiles = refusal_profiles(&["MNQ", "BTCUSDT"]);
        assert_eq!(profiles.configured_symbols(), ["BTCUSDT", "MNQ"]);
        assert!(profiles.configured("BTCUSDT").is_some());
        assert!(
            profiles.configured("btcusdt").is_none(),
            "the configured map stays as case-exact as synthesis"
        );
    }

    #[test]
    fn an_absent_socket_symbol_binds_the_boot_symbol() {
        let bound: mogwai_protocol::Symbol = Arc::from("MNQ");
        assert_eq!(
            resolve_socket_symbol(None, &bound).expect("default symbol"),
            bound
        );
    }

    #[test]
    fn a_socket_symbol_matching_the_boot_symbol_binds_it() {
        let bound: mogwai_protocol::Symbol = Arc::from("MNQ");
        assert_eq!(
            resolve_socket_symbol(Some("MNQ"), &bound).expect("matching symbol"),
            bound
        );
    }

    #[test]
    fn a_miscased_socket_symbol_is_a_distinct_resolved_label() {
        let bound: mogwai_protocol::Symbol = Arc::from("MNQ");
        let resolved = resolve_socket_symbol(Some("mnq"), &bound).unwrap();
        assert_eq!(resolved.as_ref(), "mnq");
    }

    #[test]
    fn a_configured_non_boot_socket_symbol_resolves_its_river() {
        let bound: mogwai_protocol::Symbol = Arc::from("MNQ");
        let resolved =
            resolve_socket_symbol(Some("BTCUSDT"), &bound).expect("configured river is servable");
        assert_eq!(resolved.as_ref(), "BTCUSDT");
    }

    #[test]
    fn an_unconfigured_socket_symbol_resolves_under_its_label() {
        let bound: mogwai_protocol::Symbol = Arc::from("MNQ");
        let resolved = resolve_socket_symbol(Some("MES"), &bound).unwrap();
        assert_eq!(resolved.as_ref(), "MES");
    }

    #[test]
    fn an_illegal_socket_symbol_is_refused() {
        let bound: mogwai_protocol::Symbol = Arc::from("MNQ");
        let refusal = resolve_socket_symbol(Some("MN Q"), &bound).expect_err("illegal symbol");
        assert!(refusal.contains("is not a legal symbol"), "{refusal}");
    }

    /// The refusal echoes the request, so the echo is capped: an unbounded
    /// query string must not become an unbounded response body or log line.
    #[test]
    fn an_absurd_socket_symbol_is_truncated_in_the_refusal() {
        let bound: mogwai_protocol::Symbol = Arc::from("MNQ");
        let refusal =
            resolve_socket_symbol(Some(&"X".repeat(4096)), &bound).expect_err("absurd symbol");
        assert!(refusal.contains(&"X".repeat(64)), "the echo survives");
        assert!(!refusal.contains(&"X".repeat(65)), "the echo is capped");
    }

    /// The bound is only real if the slot outlives the RESPONSE, not the
    /// handler. Reverting `history_page` to drop the permit at handler exit
    /// makes the second assertion read one permit early.
    #[tokio::test]
    async fn a_history_page_holds_its_slot_until_its_body_is_written() {
        let gate = Arc::new(tokio::sync::Semaphore::new(1));
        let slot = Arc::clone(&gate).try_acquire_owned().expect("slot");
        let response = history_page(slot, b"[]".to_vec());
        assert_eq!(gate.available_permits(), 0, "the slot is held by the page");
        let body = axum::body::to_bytes(response.into_body(), 64)
            .await
            .expect("collect the page body");
        assert_eq!(&body[..], b"[]");
        assert_eq!(
            gate.available_permits(),
            1,
            "the slot returns only once the body is gone"
        );
    }

    /// The worst-case resident cost of one admitted page, which is what
    /// `MAX_CONCURRENT_HISTORY_REQUESTS` multiplies. `/quotes` is the wider
    /// endpoint: four decimals against the trade's two.
    #[test]
    #[ignore = "measurement instrument"]
    fn worst_case_history_page_bytes() {
        let symbol = mogwai_protocol::Symbol::from("MNQZ5");
        let quotes = (0..MAX_HISTORY_LIMIT)
            .map(|i| QuoteTick {
                symbol: Arc::clone(&symbol),
                bid_px: rust_decimal::Decimal::new(2_100_025 + i as i64, 2),
                ask_px: rust_decimal::Decimal::new(2_100_050 + i as i64, 2),
                bid_sz: rust_decimal::Decimal::new(37, 0),
                ask_sz: rust_decimal::Decimal::new(42, 0),
                ts_event: 1_700_000_000_000_000_000 + i as u64,
            })
            .collect::<Vec<_>>();
        let trades = (0..MAX_HISTORY_LIMIT)
            .map(|i| TradeTick {
                symbol: Arc::clone(&symbol),
                price: rust_decimal::Decimal::new(2_100_025 + i as i64, 2),
                size: rust_decimal::Decimal::new(37, 0),
                aggressor: mogwai_protocol::AggressorSide::Buyer,
                ts_event: 1_700_000_000_000_000_000 + i as u64,
            })
            .collect::<Vec<_>>();
        eprintln!(
            "quote_vec_bytes={} quote_json_bytes={} trade_vec_bytes={} trade_json_bytes={}",
            quotes.len() * std::mem::size_of::<QuoteTick>(),
            serde_json::to_vec(&quotes).expect("serialize quotes").len(),
            trades.len() * std::mem::size_of::<TradeTick>(),
            serde_json::to_vec(&trades).expect("serialize trades").len(),
        );
    }

    #[test]
    #[ignore = "measurement instrument"]
    fn history_admission_overhead() {
        const ITERATIONS: usize = 1_000_000;
        let gate = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_HISTORY_REQUESTS));
        let started = std::time::Instant::now();
        for _ in 0..ITERATIONS {
            drop(Arc::clone(&gate).try_acquire_owned().expect("slot"));
        }
        let elapsed = started.elapsed();
        eprintln!(
            "iterations={ITERATIONS} elapsed_ns={} ns_per_admission={}",
            elapsed.as_nanos(),
            elapsed.as_nanos() / ITERATIONS as u128
        );
    }
}

/// The single owner of `/trades` page-size policy: a missing `limit` requests a
/// full page, and any requested size is clamped to `MAX_HISTORY_LIMIT`. Folding
/// the `unwrap_or` + `.min()` here (rather than splitting the clamp into the
/// handler and an empty-vec guard into `bounded_trades`) keeps the page-size
/// contract in one place. A normalized `0` still flows through to the early
/// return in `bounded_trades`, which the synthesis loop relies on (the
/// `out.len() >= limit` break would otherwise yield one tick for `limit == 0`).
pub(crate) fn normalize_limit(limit: Option<usize>) -> usize {
    limit
        .unwrap_or(DEFAULT_HISTORY_LIMIT)
        .min(MAX_HISTORY_LIMIT)
}

pub(crate) fn bounded_trades(
    symbol: &str,
    start: Option<u64>,
    end: Option<u64>,
    limit: usize,
    rivers: &source::Rivers,
) -> anyhow::Result<Vec<TradeTick>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let mut merged = rivers.history_source(symbol, start)?;
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

    Ok(out)
}

pub(crate) fn bounded_quotes(
    symbol: &str,
    start: Option<u64>,
    end: Option<u64>,
    limit: usize,
    rivers: &source::Rivers,
) -> anyhow::Result<Vec<QuoteTick>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut merged = rivers.history_source(symbol, start)?;
    let mut out = Vec::new();
    while let Some(tick) = merged.next_tick() {
        if let Some(end) = end
            && tick.ts_event() > end
        {
            break;
        }
        let TickEvent::Quote(quote) = tick else {
            continue;
        };
        // No `>= start` guard, and deliberately none: `history_source` hands
        // `start` to `MergeSource::starting_at`, which seeks each child and
        // retains the first tick AT OR AFTER it, and every `seek_to` - the
        // default drain and `GeneratedSource`'s checkpoint-skipping one alike -
        // returns only `ts_event >= start_ts`. Nothing this loop sees can
        // precede `start`. A guard here used to compensate for a contract that
        // already holds, and its asymmetry with `bounded_trades` read as one of
        // the two being wrong.
        out.push(quote);
        if out.len() >= limit {
            break;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod calendar_tests {
    use super::*;
    use mogwai_data::{SessionCalendar, WeeklyWindow};
    use mogwai_protocol::{Side, SubmitOrder, TimeInForce};
    use rust_decimal::Decimal;

    fn generated_profiles() -> std::sync::Arc<source::Rivers> {
        crate::fills::test_rivers()
    }

    #[test]
    fn bounded_quotes_respects_the_window_and_the_limit() {
        let profiles = generated_profiles();
        let first = bounded_quotes("BTCUSDT", Some(0), None, 4, &profiles).unwrap();
        assert_eq!(first.len(), 4);
        assert!(
            first
                .windows(2)
                .all(|pair| pair[0].ts_event <= pair[1].ts_event)
        );
        let start = first[1].ts_event;
        let end = first[2].ts_event;
        let bounded = bounded_quotes("BTCUSDT", Some(start), Some(end), 10, &profiles).unwrap();
        assert!(!bounded.is_empty());
        assert!(
            bounded
                .iter()
                .all(|quote| (start..=end).contains(&quote.ts_event))
        );
    }

    /// The contract `bounded_quotes` dropped its own `>= start` guard onto:
    /// a start instant landing in a GAP between ticks - the case an
    /// on-a-tick start cannot distinguish - still yields nothing earlier than
    /// it, on both routes. If a seek ever starts returning the tick BEFORE the
    /// target, this is what says so.
    #[test]
    fn a_mid_gap_start_yields_nothing_earlier_on_either_route() {
        let profiles = generated_profiles();
        let quotes = bounded_quotes("BTCUSDT", Some(0), None, 8, &profiles).unwrap();
        let trades = bounded_trades("BTCUSDT", Some(0), None, 8, &profiles).unwrap();
        assert!(quotes.len() > 2 && trades.len() > 2);

        // Strictly inside the gap between two prints, so the seek cannot land
        // on the boundary by luck.
        let gap = |before: u64, after: u64| {
            assert!(after > before + 1, "adjacent prints leave no gap to aim at");
            before + 1
        };

        let start = gap(quotes[0].ts_event, quotes[1].ts_event);
        assert!(
            bounded_quotes("BTCUSDT", Some(start), None, 8, &profiles)
                .unwrap()
                .iter()
                .all(|quote| quote.ts_event >= start)
        );

        let start = gap(trades[0].ts_event, trades[1].ts_event);
        assert!(
            bounded_trades("BTCUSDT", Some(start), None, 8, &profiles)
                .unwrap()
                .iter()
                .all(|trade| trade.ts_event >= start)
        );
    }

    #[test]
    fn bounded_quotes_reproduce_the_live_quote_sequence() {
        let profiles = generated_profiles();
        let history = bounded_quotes("BTCUSDT", Some(0), None, 100, &profiles).unwrap();
        let mut live = profiles
            .history_source("BTCUSDT", Some(source::TAPE_ORIGIN_NS))
            .expect("live source");
        let mut live_quotes = Vec::new();
        while live_quotes.len() < history.len() {
            if let TickEvent::Quote(quote) = live.next_tick().unwrap() {
                live_quotes.push(quote);
            }
        }
        assert_eq!(
            serde_json::to_string(&history).unwrap(),
            serde_json::to_string(&live_quotes).unwrap()
        );
    }

    #[test]
    fn the_quotes_route_is_no_longer_empty() {
        assert!(
            !bounded_quotes("BTCUSDT", Some(0), None, 1, &generated_profiles())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_market_order_while_closed_is_rejected_not_filled_off_a_stale_print() {
        let calendar = SessionCalendar {
            utc_offset_minutes: 0,
            // Minutes count from local SUNDAY 00:00, and the unix epoch is a
            // Thursday, so an open window covering `ts = 0` starts at 5_760.
            open_windows: vec![WeeklyWindow {
                start_minute: 5_760,
                end_minute: 5_761,
            }],
            settlement_minute_of_day: None,
        };
        let order = SubmitOrder {
            client_order_id: "CLOSED".into(),
            symbol: "MNQ".into(),
            position_id: None,
            side: Side::Buy,
            order_type: OrderType::Market,
            quantity: Decimal::ONE,
            price: None,
            trigger_price: None,
            reduce_only: false,
            post_only: false,
            time_in_force: TimeInForce::Gtc,
        };
        let reading = Some(mogwai_engine::MarketReading {
            last_px: Decimal::from(21_000),
            ts_ns: 0,
            band_ticks: 0,
        });
        let closed = 2 * 60_000_000_000;
        let open = 30_000_000_000;
        assert!(reject_while_closed(&calendar, closed, &order, reading));

        // The three negative cases, without which the assertion above passes
        // for a predicate that answers `true` unconditionally.
        assert!(
            !reject_while_closed(&calendar, open, &order, reading),
            "an open market must not refuse a market order"
        );
        let mut resting = order.clone();
        resting.order_type = OrderType::Limit;
        resting.price = Some(Decimal::from(1));
        assert!(
            !reject_while_closed(&calendar, closed, &resting, reading),
            "a non-marketable limit rests through a closure rather than being refused"
        );
        let mut marketable = resting.clone();
        marketable.price = Some(Decimal::from(30_000));
        assert!(
            reject_while_closed(&calendar, closed, &marketable, reading),
            "a limit marketable against the stale print is refused with the market orders"
        );
    }
}
