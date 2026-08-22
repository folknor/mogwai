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
    response::{IntoResponse, Response},
};
use mogwai_data::TickEvent;
use mogwai_engine::MAX_ARMED_DIVERGENCES;
use mogwai_protocol::{
    AccountState, AdmissionSubject, Command, CommandClass, DEFAULT_HISTORY_LIMIT, InstrumentDef,
    MAX_HISTORY_LIMIT, OrderType, QuoteTick, SimClock, TradeTick, VenueClock, VenueMessage,
    control::Divergence, trades_through, truncate_echoed_id, truncate_reason,
    validate_client_order_id, validate_divergence, validate_modify_order, validate_request_id,
    validate_submit_order,
};
use serde::{Deserialize, Serialize};

use crate::admission::{ExecLanes, Reservation};
use crate::config::{Config, now_ns, sim_duration_from_millis, sim_now_ns};
use crate::run::{Run, VenueArm};
use crate::source;

#[derive(Deserialize)]
pub(crate) struct DivergenceRequest {
    #[serde(default)]
    symbol: Option<String>,
    /// Which account a TRANSPORT divergence corrupts the view of. Absent means
    /// every account, which is what an operator on a single-account venue wants
    /// and what every existing scenario file already writes.
    ///
    /// Only the transport arms honour it. `GoDark` and `StallData` change what
    /// one connection RECEIVES, so they blur the eyesight of every passenger
    /// under that account;
    /// generator arms change the WATER, which is a property of the river and
    /// reaches everyone reading it whatever account they trade.
    #[serde(default)]
    account: Option<String>,
    #[serde(flatten)]
    divergence: Divergence,
}

/// The accounts one control-plane request applies to: the named account, or
/// every account when it names none.
///
/// The account a control request targets, as `Run::arm` takes it.
///
/// NAMING AN ACCOUNT NEVER CREATES ONE. Arming a blackout is not a reason to
/// bring a ledger into existence - a typo would otherwise mint `WYRD-01` and
/// arm a ledger nothing ever connects to, and would answer the real consumer's own
/// `POST /account` with a `409`. The arm is RECORDED against the name instead,
/// and the account's first mint consumes it; see `VenueArms`. A stale line here
/// said the request "resolves to the EMPTY set", which was the finding-5
/// behaviour and stopped being true when the record landed.
///
/// `None` is not "all the ledgers that exist", it is THE VENUE: the arm is
/// recorded on the run, so a late-connecting account gets it too.
///
/// A MALFORMED id IS REFUSED rather than quietly targeting nothing. An id that
/// cannot be parsed can never be presented by a socket, so no ledger will ever
/// carry the arm; the arm used to filter to the empty set and answer `202`,
/// which is the same silent success finding 5 was about. Reading it as
/// "unqualified" instead would be worse still - a typo would arm the whole
/// venue - so the request is a `400`, beside the divergence validation, before
/// anything is stored.
fn arm_target(account: Option<&str>) -> Result<Option<&str>, String> {
    match account {
        None => Ok(None),
        Some(named) => mogwai_protocol::AccountId::parse(named)
            .map(|_| Some(named))
            .map_err(|err| format!("account: {err}")),
    }
}

/// What one order-entry command came to. Every variant that carries frames also
/// carries the reservation those frames were produced under: there is no path
/// where a frame exists against no reservation.
pub(crate) enum OrderOutcome {
    /// The engine processed the command and produced these events.
    Produced {
        events: Vec<VenueMessage>,
        reservation: Reservation,
    },
    /// The protocol boundary refused it before the engine ever saw it. These
    /// are engine-free frames and are charged like any other output.
    Refused {
        events: Vec<VenueMessage>,
        reservation: Reservation,
    },
    /// Admission refused: outbound capacity could not cover the worst case, so
    /// the engine was never asked. Carries the frame for the PRIORITY lane.
    NotAdmitted(VenueMessage),
    /// A malformed request with no order-shaped frame to answer it (a query
    /// whose `request_id` is over-length names no order), so the answer is the
    /// untargeted diagnostic. Also a priority-lane frame, but deliberately NOT
    /// an `AdmissionRejected`: conflating malformed with over-capacity would
    /// make an admission refusal unreadable as a load signal.
    Diagnostic(VenueMessage),
}

/// Name what a refusal refused, so the consumer can translate it per command -
/// a refused cancel must not read as a rejected order.
pub(crate) fn admission_subject(cmd: &Command) -> AdmissionSubject {
    match cmd {
        Command::SubmitOrder(o) => AdmissionSubject::Submit {
            client_order_id: o.client_order_id.clone(),
        },
        // Named by the LIST, because a group is admitted or refused whole.
        // A group whose first member carries no link is one
        // `validate_submit_group` refuses anyway, so the empty id here is a
        // subject for a frame that only ever accompanies that refusal.
        Command::SubmitOrderGroup { orders } => AdmissionSubject::SubmitGroup {
            order_list_id: orders
                .first()
                .and_then(|order| order.link.as_ref())
                .map(|link| link.order_list_id.clone())
                .unwrap_or_default(),
        },
        Command::CancelOrder { client_order_id } => AdmissionSubject::Cancel {
            client_order_id: client_order_id.clone(),
        },
        Command::ModifyOrder {
            client_order_id, ..
        } => AdmissionSubject::Modify {
            client_order_id: client_order_id.clone(),
        },
        Command::QueryOrders { request_id, .. } => AdmissionSubject::Query {
            request_id: request_id.clone(),
            query: mogwai_protocol::QueryKind::Orders,
        },
        Command::QueryFills { request_id, .. } => AdmissionSubject::Query {
            request_id: request_id.clone(),
            query: mogwai_protocol::QueryKind::Fills,
        },
    }
}

/// The protocol-boundary verdict on one order-entry command: `Some(reason)`
/// when it is malformed. Ids are length-checked here, alongside the numeric
/// checks that were always here, because both are malformed-request failures.
pub(crate) fn boundary_error(cmd: &Command) -> Option<&'static str> {
    match cmd {
        // A LINKED order may not arrive alone on the wire, and this is the
        // refusal that makes the group frame's guarantee worth anything.
        //
        // Per-leg dispatch of a bracket is the whole hazard: leg one can FILL
        // before leg two is admitted, the rule adjusts a sibling that is not
        // there, and leg two then arrives at full size - so a two-leg `Ouo`
        // pair's aggregate fill is twice the bracket quantity, which for a
        // crossed slice reverses the account. A venue that served that route
        // beside the atomic one could not state the atomicity as a property of
        // linkage at all, only as a property of one code path, and a consumer
        // cannot cite that.
        //
        // The ENGINE still accepts a linked standalone submit: it is the
        // in-process API the group path itself is built on, and the venue's own
        // liquidation submits go through it. This is a WIRE rule, refused where
        // the consumer's choice of route is actually made.
        Command::SubmitOrder(order) => validate_submit_order(order).err().or_else(|| {
            order.link.is_some().then_some(
                "a linked order must be submitted as part of a SubmitOrderGroup: sent alone, a \
                 sibling can fill before the rest of the group is admitted",
            )
        }),
        Command::SubmitOrderGroup { orders } => {
            mogwai_protocol::validate_submit_group(orders).err()
        }
        Command::ModifyOrder {
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
        Command::CancelOrder { client_order_id } => validate_client_order_id(client_order_id).err(),
        Command::QueryOrders {
            request_id,
            client_order_id,
            ..
        }
        | Command::QueryFills {
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

/// The refusal frames for a malformed order-entry command, echoing the
/// offending id TRUNCATED to `MAX_ECHOED_ID_LEN`. Echoing it at full length
/// would turn an 8 MiB `client_order_id` into an 8 MiB `OrderRejected`,
/// recreating exactly the unbounded frame the cap exists to prevent; a
/// truncated echo cannot be mistaken for a live correlation because the venue
/// would never have accepted the id under either spelling, and the reason says
/// so.
///
/// A VEC rather than one frame, because a `SubmitOrderGroup` is refused whole
/// and owes every member its own rejection. Every other command answers with
/// exactly one, which is what `boundary_frame_count` states and what the
/// reservation is sized from.
fn boundary_refusal(cmd: &Command, reason: &str, ts: u64) -> Vec<VenueMessage> {
    let note = |id: &str| {
        if id.len() > mogwai_protocol::MAX_ECHOED_ID_LEN {
            format!(
                "{reason}; the identifier was truncated for display and no order \
                 exists under either spelling"
            )
        } else {
            reason.to_string()
        }
    };
    match cmd {
        Command::ModifyOrder {
            client_order_id, ..
        } => vec![VenueMessage::OrderModifyRejected {
            client_order_id: truncate_echoed_id(client_order_id.clone()),
            venue_order_id: None,
            reason: note(client_order_id),
            ts_event: ts,
        }],
        Command::CancelOrder { client_order_id } => {
            vec![VenueMessage::OrderCancelRejected {
                client_order_id: truncate_echoed_id(client_order_id.clone()),
                venue_order_id: None,
                reason: note(client_order_id),
                ts_event: ts,
            }]
        }
        Command::SubmitOrder(order) => vec![VenueMessage::OrderRejected {
            client_order_id: truncate_echoed_id(order.client_order_id.clone()),
            reason: note(&order.client_order_id),
            ts_event: ts,
        }],
        // A group is refused WHOLE, so every member gets its own rejection.
        // Answering with one frame naming one member would leave the consumer
        // waiting on the others, and answering with none would leave it waiting
        // on all of them. An EMPTY group has no member to answer, so it falls
        // through to the untargeted diagnostic like a malformed query.
        Command::SubmitOrderGroup { orders } if !orders.is_empty() => orders
            .iter()
            .map(|order| VenueMessage::OrderRejected {
                client_order_id: truncate_echoed_id(order.client_order_id.clone()),
                reason: note(&order.client_order_id),
                ts_event: ts,
            })
            .collect(),
        // Queries have no order-shaped rejection frame; the caller answers
        // these with the untargeted diagnostic instead.
        _ => vec![VenueMessage::ProtocolError {
            reason: reason.to_string(),
            ts_event: ts,
        }],
    }
}

/// The orders a command SUBMITS: one for a `SubmitOrder`, every member for a
/// `SubmitOrderGroup`, none for anything else.
///
/// It exists so the pre-engine refusals below - bound symbol, policy currency,
/// locked account, position cap, market synthesis, session closed - are written
/// ONCE over both carriers. A guard that only looked at `SubmitOrder` would let
/// a group walk past it, and every one of those guards is a rule about capital
/// rather than a formality.
fn submitted_orders(cmd: &Command) -> &[mogwai_protocol::SubmitOrder] {
    match cmd {
        Command::SubmitOrder(order) => std::slice::from_ref(order),
        Command::SubmitOrderGroup { orders } => orders,
        _ => &[],
    }
}

/// Refuse everything a command submitted, blaming `blamed` for the reason.
///
/// A group is refused WHOLE, so one bad member rejects all of them - which is
/// the same rule the atomic-admission guarantee states, applied at the pre-engine
/// guards rather than inside the engine. Each frame names the member it is
/// addressed to, and a group's frames say which member was actually at fault so
/// a consumer is not left guessing which leg to fix.
fn refuse_submitted(cmd: &Command, blamed: &str, reason: &str, ts: u64) -> Vec<VenueMessage> {
    let orders = submitted_orders(cmd);
    orders
        .iter()
        .map(|order| VenueMessage::OrderRejected {
            client_order_id: order.client_order_id.clone(),
            reason: truncate_reason(if orders.len() > 1 && order.client_order_id != blamed {
                format!("order group rejected whole: {blamed} was refused because {reason}")
            } else {
                reason.to_owned()
            }),
            ts_event: ts,
        })
        .collect()
}

/// Reserve for, and build, the refusal of everything `cmd` submitted.
///
/// One place rather than one per guard, because each guard owes exactly the
/// same three steps - reserve for as many frames as there are members, refuse
/// them all, blame the offender - and writing them out six times is how a group
/// ends up refused with one frame and a consumer left waiting on the rest.
fn refuse_all(
    cmd: &Command,
    lanes: &ExecLanes,
    blamed: &str,
    reason: &str,
    ts: u64,
) -> OrderOutcome {
    let Some(reservation) = lanes.try_reserve_boundary_frames(submitted_orders(cmd).len()) else {
        return OrderOutcome::NotAdmitted(VenueMessage::AdmissionRejected {
            subject: admission_subject(cmd),
            reason: "execution output admission budget exhausted".into(),
            retryable: true,
            ts_event: ts,
        });
    };
    OrderOutcome::Refused {
        events: refuse_submitted(cmd, blamed, reason, ts),
        reservation,
    }
}

/// How many boundary refusal frames `cmd` can produce, which is what its
/// reservation is sized from. One for everything except a group, which owes one
/// per member and is bounded by `MAX_GROUP_ORDERS`.
fn boundary_frame_count(cmd: &Command) -> usize {
    match cmd {
        Command::SubmitOrderGroup { orders } => orders.len().max(1),
        _ => 1,
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
///    is not the engine's to blame on the consumer.
/// 4. Take the engine lock, read `book_shape()`, reserve worst-case output.
///    The lock spans the shape read and the processing, so the shape cannot
///    drift out from under the reservation that covers it.
/// 5. Only then `engine.process`. If the reservation failed, the engine was
///    never asked, so nothing mutated: no venue order id burned, no order
///    resting.
///
/// The websocket order carrier uses this one gate for every command. `None`
/// means the command cleared the protocol boundary.
fn boundary_outcome(order_cmd: &Command, lanes: &ExecLanes, ts: u64) -> Option<OrderOutcome> {
    let reason = boundary_error(order_cmd)?;
    let Some(reservation) = lanes.try_reserve_boundary_frames(boundary_frame_count(order_cmd))
    else {
        return Some(OrderOutcome::NotAdmitted(VenueMessage::AdmissionRejected {
            subject: admission_subject(order_cmd),
            reason: "execution output admission budget exhausted".into(),
            retryable: true,
            ts_event: ts,
        }));
    };
    let mut events = boundary_refusal(order_cmd, reason, ts);
    if events.len() == 1 && matches!(events[0], VenueMessage::ProtocolError { .. }) {
        // The reservation is not needed after all: nothing goes on the
        // held lane, so drop it and give the bytes straight back.
        drop(reservation);
        return Some(OrderOutcome::Diagnostic(events.remove(0)));
    }
    Some(OrderOutcome::Refused {
        events,
        reservation,
    })
}

pub(crate) async fn process_order_cmd(
    order_cmd: Command,
    state: &AppState,
    run: &Arc<Run>,
    lanes: &ExecLanes,
    socket_symbol: &mogwai_protocol::Symbol,
    boat: &Arc<crate::boatyard::Boat>,
    account_state: &crate::run::Account,
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
    // consumer could bind one river and trade another.
    //
    // PLACEMENT IS THE CONTRACT: right after the protocol boundary and BEFORE
    // the act delay, the market reading, the calendar lookup and the engine
    // lock. A check placed lower would let a mismatched MARKET order drive
    // price synthesis, checkpoint-mutex and cache work for a river the socket
    // is not bound to before being refused. Refusing here also dates the
    // refusal at the entry-time `ts`, like its boundary neighbours.
    //
    // The frame is `OrderRejected`, NOT `AdmissionRejected`: a mismatch is a
    // consumer error, and `AdmissionRejected` reads as a capacity signal at every
    // observer. Only the failure to reserve a boundary slot is capacity.
    //
    // EVERY member of a group is checked, and one mismatch refuses all of them.
    // The same is true of every guard below: a group is refused whole, which is
    // the atomic-admission rule applied at the pre-engine gates rather than only
    // inside the engine.
    if let Some(order) = submitted_orders(&order_cmd)
        .iter()
        .find(|order| order.symbol.as_ref() != socket_symbol.as_ref())
    {
        let echoed: String = order.symbol.chars().take(MAX_ECHOED_SYMBOL).collect();
        return refuse_all(
            &order_cmd,
            lanes,
            &order.client_order_id.clone(),
            &format!(
                "order symbol {echoed} does not match the symbol this connection is bound to \
                 ({socket_symbol})"
            ),
            ts,
        );
    }
    // A POLICED ACCOUNT TRADES ONLY WHAT ITS POLICY CAN VALUE, refused here
    // rather than discovered as a mis-valued threshold later.
    //
    // Equity is stated in the policy's currency, and the venue prices an asset
    // only through an instrument that QUOTES it in that currency. A future
    // settling in it qualifies, and so does a spot pair quoted in it - buying
    // BTC on BTCUSDT under a USDT policy leaves a BTC balance the BTCUSDT mark
    // can value. What does not qualify is a shape that would leave a holding
    // nothing prices in the policy currency, and asking for one is a consumer
    // error with a reason that says why.
    if let Some(currency) = account_state
        .risk
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .currency()
        .map(str::to_owned)
        && let Some(order) = submitted_orders(&order_cmd).iter().find(|order| {
            run.rivers
                .resolve_profile(&order.symbol)
                .is_ok_and(|profile| !settles_only_in(&profile.def, &currency))
        })
    {
        let echoed: String = order.symbol.chars().take(MAX_ECHOED_SYMBOL).collect();
        return refuse_all(
            &order_cmd,
            lanes,
            &order.client_order_id.clone(),
            &format!(
                "account {account} is policed in {currency} and {echoed} would make it hold \
                 another currency; the venue has no rate to state its equity with, so a policed \
                 account trades only shapes settling in its own currency",
                account = account_state.account_id.as_str()
            ),
            ts,
        );
    }
    // A LOCKED ACCOUNT OPENS NOTHING. This is the other half of enforcement: the
    // breach flattened the book, and this is what stops the strategy putting it
    // straight back on. Placed with the other consumer-error refusals and BEFORE
    // the act delay and market reading, because a locked account's order needs
    // no price to be refused.
    //
    // Cancels and queries are deliberately still served: a locked consumer must
    // still be able to see and tidy its own book, and refusing a query would
    // make a locked account indistinguishable from a broken one.
    if let Some(order) = submitted_orders(&order_cmd).first()
        && account_state
            .risk
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_locked()
    {
        return refuse_all(
            &order_cmd,
            lanes,
            &order.client_order_id.clone(),
            &format!(
                "account {account} breached its risk policy and may not open a position",
                account = account_state.account_id.as_str()
            ),
            ts,
        );
    }
    // Register the symbol lazily, BEFORE the act delay and any market reading.
    // A `false` here means no profile resolves the symbol; fall through rather
    // than short-circuit, so an unconfigured symbol still meets the engine's
    // existing unknown-instrument rejection with its existing wording instead of
    // a second one invented here.
    //
    // A group names ONE symbol - `validate_submit_group` refuses anything else -
    // so registering the first member's registers the group's.
    if let Some(order) = submitted_orders(&order_cmd).first() {
        let _configured = run.ensure_instrument(account_state, &order.symbol).await;
    }
    // A POSITION CAP is refused here, by name, before the act delay. An
    // oversized submit is a consumer error: the firm would not have taken the
    // order, so the venue must not either. Reduce-only never grows the book
    // and is left alone.
    //
    // The risk guard is dropped BEFORE the engine await: holding a
    // `std::sync::Mutex` across `.await` makes this future `!Send`, and the
    // socket task has to be Send.
    let position_cap = account_state
        .risk
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .max_position();
    //
    // A GROUP IS JUDGED ON ITS WHOLE WORST CASE, not member by member. Every
    // opening member's quantity is summed onto the projection before the cap is
    // compared, because the members are admitted together and the venue must not
    // admit a pair that individually fits and jointly does not - which is the
    // same worst-case-fill-order reading the cap already takes of a working
    // book. Reduce-only members contribute nothing, since they never grow it.
    if let Some(cap) = position_cap {
        let additional: rust_decimal::Decimal = submitted_orders(&order_cmd)
            .iter()
            .filter(|order| !order.reduce_only)
            .map(|order| match order.side {
                mogwai_protocol::Side::Buy => order.quantity,
                mogwai_protocol::Side::Sell => -order.quantity,
            })
            .sum();
        if let Some(order) = submitted_orders(&order_cmd).first()
            && !additional.is_zero()
        {
            let projected = account_state
                .engine
                .lock()
                .await
                .projected_qty(&order.symbol, additional);
            if projected > cap {
                return refuse_all(
                    &order_cmd,
                    lanes,
                    &order.client_order_id.clone(),
                    &format!(
                        "account {account} may not carry more than {cap} of {symbol}; this order \
                         would take the book to {projected}",
                        account = account_state.account_id.as_str(),
                        symbol = order.symbol,
                    ),
                    ts,
                );
            }
        }
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
    let act_ms = class.map_or(0, |class| account_state.act_ms(class));
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
    // Reject it here with the honest story - the consumer correctly sent no price
    // (nautilus never stamps a market order), so letting the engine's "submit
    // price required" fire would blame the consumer for the venue's own synthesis
    // failure. A symbol the resolver REFUSES - illegal, funding-barred, or one
    // whose resolved shape does not validate - is deliberately left price-less:
    // the engine checks instrument existence before the price, so its "unknown
    // instrument" rejection tells that story unaltered. Resolution is otherwise
    // total, so "does the venue list it" is now "did the resolve succeed".
    if let Some(order) = submitted_orders(&order_cmd).iter().find(|order| {
        order.order_type == OrderType::Market
            && order.price.is_none()
            && state.rivers.resolve_profile(&order.symbol).is_ok()
    }) {
        tracing::warn!(
            symbol = %order.symbol,
            client_order_id = %order.client_order_id,
            "rejecting price-less market order: market price synthesis failed"
        );
        return refuse_all(
            &order_cmd,
            lanes,
            &order.client_order_id.clone(),
            "venue could not synthesize a market price at sim-now",
            ts,
        );
    }
    if let Some(order) = submitted_orders(&order_cmd).iter().find(|order| {
        // The RESOLVED shape's calendar, not the configured map's: a symbol
        // nobody configured is served on a resolved bundle whose tape is
        // generated with that calendar, so its orders owe the same
        // session-closed refusal.
        state
            .rivers
            .resolve_profile(&order.symbol)
            .ok()
            .and_then(|profile| profile.calendar.clone())
            .is_some_and(|calendar| reject_while_closed(&calendar, ts, order, market_px))
    }) {
        return refuse_all(
            &order_cmd,
            lanes,
            &order.client_order_id.clone(),
            "market closed",
            ts,
        );
    }
    let mut engine = account_state.engine.lock().await;
    let shape = engine.book_shape();
    let Some(reservation) = lanes.reserve(&order_cmd, &shape) else {
        // The engine has NOT been asked to process anything, so nothing
        // mutated: the refusal is the whole effect of this command.
        return OrderOutcome::NotAdmitted(VenueMessage::AdmissionRejected {
            subject: admission_subject(&order_cmd),
            reason: "execution output admission budget exhausted".into(),
            retryable: true,
            ts_event: ts,
        });
    };
    let events = engine.process_with_market_on_clock(order_cmd, ts, market_px, sim);
    // RELEASED BEFORE THESE EVENTS ARE PUBLISHED, which makes the order visible
    // to the sweeper while its own `OrderAccepted` is still in this vec. The
    // engine mutex establishes MUTATION order; it does not establish
    // PUBLICATION order, and nothing else does either. See the note at the
    // `submit_produced` call in `ws::dispatch_command` for why that is currently
    // harmless and what would stop it being so.
    drop(engine);
    OrderOutcome::Produced {
        events,
        reservation,
    }
}

/// Would this order execute off a print the closure has made stale?
///
/// THIS ENUMERATES ORDER TYPES, so a type whose semantics change owes this
/// function a re-read. `MarketToLimit` cost exactly that on 2026-08-19: it used
/// to fill at its own stated price with no reference to the tape, so its
/// absence here was defensible, and the moment it started taking the market it
/// became at least as aggressive as a marketable limit and at most as
/// aggressive as a market order - a marketable one would have been admitted and
/// filled against a stale reading, which is the one thing this guard exists to
/// prevent.
///
/// Marketability is judged against the STATED price, while the engine judges it
/// against the band-drawn trigger. The two can disagree by up to the band, so
/// this is an approximation in both directions: an order admitted here as
/// non-marketable can be marketable to the engine (and fill off the stale
/// print this guard exists to refuse), and one refused here can be one the
/// engine would have rested. The engine's `draw_trigger` needs the order's
/// `band_ticks` and the run's `fill_seed`, neither of which this boundary
/// holds, so closing the gap means asking the engine the question rather than
/// re-deriving it. It is the pre-existing `Limit` behaviour and is not made
/// worse by sharing it.
fn reject_while_closed(
    calendar: &mogwai_data::SessionCalendar,
    ts: u64,
    order: &mogwai_protocol::SubmitOrder,
    market_px: Option<mogwai_engine::MarketReading>,
) -> bool {
    // A market order takes whatever the tape last said, so a closure refuses it
    // outright. Every type that STATES A LIMIT - the limit itself and the
    // market-to-limit, whose first act is judged by its limit exactly as a
    // limit's is - is refused only when that limit is marketable against the
    // stale print, and rests through the closure otherwise.
    let states_a_limit = matches!(
        order.order_type,
        OrderType::Limit | OrderType::MarketToLimit
    );
    !calendar.is_open(ts)
        && (order.order_type == OrderType::Market
            || (states_a_limit
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
    /// delay. The per-connection lane bounds one consumer; this bounds the run.
    pub(crate) pending_commands: Arc<tokio::sync::Semaphore>,
    /// Slots for history syntheses IN FLIGHT, which is what bounds resident
    /// page memory. A caller over the cap WAITS for one; see
    /// `acquire_history_slot`.
    pub(crate) history_slots: Arc<tokio::sync::Semaphore>,
    /// Callers QUEUED for one of those slots and holding no page yet. The
    /// waiter permit is given up the instant a slot is won, so this counts
    /// waiting and never synthesis. This is the fail-fast half, and the only
    /// one that still refuses on contention.
    pub(crate) history_slot_waiters: Arc<tokio::sync::Semaphore>,
}

#[derive(Serialize)]
pub(crate) struct Health {
    status: &'static str,
    oms_type: mogwai_protocol::OmsType,
    /// Identifies this RUN, not this process.
    ///
    /// The endpoint is an ephemeral port, and a port outlives nothing: once a
    /// venue exits, the number is free and anything may take it. A consumer
    /// holding that address has no way to tell the run it was launched against
    /// from whatever now answers there, and the window is not hypothetical -
    /// this venue stops accepting BEFORE it exits, draining live connections for
    /// up to the shutdown grace, so the port is free while the process is still
    /// alive and any consumer watching for child exit sees nothing.
    ///
    /// The seed is already unique per run and already reported in the readiness
    /// record, so a launcher can hand it to its consumers and they can check they
    /// are still talking to the venue they were given.
    run_seed: u64,
    /// A FAULTED TAPE ON ANY BOATED RIVER, not merely on the boot one.
    ///
    /// This used to read `boat_for_symbol(boot_symbol)` and report that one
    /// boat's tape fault, which was exactly right when a run had one paced
    /// tape. Under the open instrument set a run places a boat per keyed river,
    /// every one owns its own tape and can fault independently, and the boot
    /// river is the one a strategy under test is LEAST likely to have bound -
    /// so a consumer whose own arrival draw refused was reading a healthy
    /// `/health`. That is not cosmetic: a launcher and an orchestrator poll
    /// this to decide whether a fire-and-forget run is worth keeping, and a
    /// fault that never reaches the poll gets the run scored healthy and its
    /// output silently trusted.
    ///
    /// ONE OPTIONAL OBJECT, N BOATS, so the choice is which boat answers. It is
    /// the faulted river with the smallest SYMBOL, which keeps the field
    /// deterministic across polls (reporting whichever the registry iterated
    /// first would not be), keeps the wire shape every existing consumer
    /// already reads, and still answers "is ANY river faulted" - the question a
    /// fleet poller actually has. `symbol` says which river it is, so the
    /// narrowing costs no information about the fault that is reported; what it
    /// does not report is a SECOND simultaneous fault, which changes no
    /// decision, because one faulted river already condemns the run.
    ///
    /// Separate from the venue's terminal fault shutdown path: this is what a
    /// poller can see BEFORE a run dies, not when it dies.
    fault: Option<HealthFault>,
}

#[derive(Serialize)]
struct HealthFault {
    /// The river that faulted. Absent from this field's earlier shape because
    /// only the boot river was ever reported.
    symbol: String,
    kind: &'static str,
    clock_ns: u64,
}

/// Which faulted river `/health` reports, given every boated river that has
/// one. Split out from the handler so the SELECTION is testable without a
/// boatyard: forcing a real arrival refusal on a chosen river is far more
/// machinery than the rule deserves, and the rule - smallest symbol wins,
/// whatever order the registry iterated in - is the whole of what could
/// regress.
fn health_fault(
    faults: impl IntoIterator<Item = (String, mogwai_data::TickFault)>,
) -> Option<HealthFault> {
    let (symbol, fault) = faults
        .into_iter()
        .min_by(|(left, _), (right, _)| left.cmp(right))?;
    Some(match fault {
        mogwai_data::TickFault::Arrival(mogwai_data::ArrivalRefusal::NoOpenExposure {
            from_ns,
        }) => HealthFault {
            symbol,
            kind: "arrival.no_open_exposure",
            clock_ns: from_ns,
        },
        mogwai_data::TickFault::Arrival(mogwai_data::ArrivalRefusal::IntensityCeiling {
            clock_ns,
            ..
        }) => HealthFault {
            symbol,
            kind: "arrival.intensity_ceiling",
            clock_ns,
        },
        mogwai_data::TickFault::Arrival(mogwai_data::ArrivalRefusal::NonFiniteState {
            clock_ns,
        }) => HealthFault {
            symbol,
            kind: "arrival.non_finite_state",
            clock_ns,
        },
        // ZERO IS THE HONEST CLOCK, not a missing one. An injected fault is not
        // dated by any source's cursor, and reporting the venue's current sim
        // instant instead would read as the moment a tape gave out - which is
        // exactly the thing that did not happen. The `kind` says where it came
        // from, so a consumer never has to infer it from the instant.
        mogwai_data::TickFault::Injected => HealthFault {
            symbol,
            kind: "injected",
            clock_ns: 0,
        },
    })
}

pub(crate) async fn health(State(state): State<AppState>) -> Json<Health> {
    Json(Health {
        status: "ok",
        oms_type: state.run.oms_type,
        run_seed: state.run.seeds.run,
        fault: health_fault(state.run.boatyard.boats().into_iter().filter_map(|boat| {
            let fault = boat.tape.fault()?;
            Some((boat.symbol().to_owned(), fault))
        })),
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
    /// is about to be boated - invisible precisely because it is well-formed.
    pub(crate) async fn river_now(&self, symbol: &str, speed: Option<f64>) -> RiverNow {
        // Wait out an in-flight placement so a racing poll does not answer off
        // the venue clock for a river about to have a boat. BEFORE the named
        // speed is resolved too: the placement being awaited may be exactly
        // the cadence that was asked for, and answering "no such boat" off the
        // venue clock is the same well-formed lie in either shape.
        let _ = self
            .run
            .boatyard
            .boat_for_symbol_awaiting_placement(symbol)
            .await;
        if let Some(speed) = speed {
            return match self.run.boatyard.boat_at(symbol, speed) {
                Some(boat) => RiverNow {
                    ns: boat.published_ns.load(Ordering::Acquire),
                    sim: boat.sim,
                    from_boat: true,
                },
                None => RiverNow::venue(self.venue_sim()),
            };
        }
        // Several cadences: the lead. `/clock?symbol=&speed=` names one.
        let boat = self
            .run
            .boatyard
            .boats_for_symbol(symbol)
            .into_iter()
            .max_by_key(|boat| boat.published_ns.load(Ordering::Acquire));
        match boat {
            Some(boat) => RiverNow {
                ns: boat.published_ns.load(Ordering::Acquire),
                sim: boat.sim,
                from_boat: true,
            },
            None => RiverNow::venue(self.venue_sim()),
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
    /// True when `sim` is a boat's. Renders as `VenueClock::boat_clock`.
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
    let target = match arm_target(request.account.as_deref()) {
        Ok(target) => target,
        Err(err) => {
            tracing::warn!(
                ?div,
                err,
                "rejecting divergence against an unusable account"
            );
            return (StatusCode::BAD_REQUEST, err);
        }
    };
    let run = &state.run;
    tracing::info!(?div, "arming divergence");
    // Validate at the arming boundary so an out-of-range knob (e.g. a
    // `PartialFillNext.fraction` outside `(0, 1]`) is rejected before it is
    // stored into venue state or armed on the engine, rather than surfacing
    // as a degenerate fill downstream.
    match div {
        Divergence::DelayAcks { ms } => {
            run.arm(target, VenueArm::DelayAcks { ms }).await;
        }
        // STORE-not-merge, like every other venue-owned window: one arm
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
            run.arm(
                target,
                VenueArm::CommandLatency {
                    submit_act_ms,
                    modify_act_ms,
                    cancel_act_ms,
                    submit_ack_ms,
                    modify_ack_ms,
                    cancel_ack_ms,
                },
            )
            .await;
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
        //
        // TARGETED at one account when the request names one, and at the VENUE
        // otherwise. Transport havoc rides the ACCOUNT, so blacking out one
        // subagent on a shared exchange must not black out the batch - and an
        // operator on a single-account venue still writes what it always did,
        // because naming no account still means everyone, now including the
        // accounts that have not connected yet.
        Divergence::GoDark { ms } => {
            let armed_ns = now_ns();
            let span_ns = sim_duration_from_millis(ms);
            run.arm(target, VenueArm::GoDark { armed_ns, span_ns })
                .await;
        }
        Divergence::StallData { ms } => {
            let armed_ns = now_ns();
            let span_ns = sim_duration_from_millis(ms);
            run.arm(target, VenueArm::StallData { armed_ns, span_ns })
                .await;
        }
        Divergence::FlowSurge {
            rate_mult,
            children_mult,
            duration_ms,
        } => {
            let symbol = if let Some(symbol) = request.symbol.as_deref() {
                symbol
            } else {
                let placed = run.boatyard.placed_symbols();
                if !placed.is_empty() {
                    return (
                        StatusCode::BAD_REQUEST,
                        format!(
                            "generator divergence requires symbol; placed boats: {}",
                            placed.join(", ")
                        ),
                    );
                }
                run.boot_symbol.as_ref()
            };
            if run.boatyard.boat_for_symbol(symbol).is_some() {
                return (
                    StatusCode::BAD_REQUEST,
                    format!(
                        "river {symbol} has a placed boat; place one whose sharing key carries generator havoc"
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
            // VENUE-WIDE WHATEVER THE REQUEST NAMED, which is why this passes
            // `None` rather than `target`: a surcharge is a statement about the
            // venue's fees, not about one trader's connection, and the wire has
            // never let a consumer be charged differently. An account opened
            // later gets it too - that is what `Run::arm`'s record is for. This
            // used to walk `run.accounts()` while a comment here claimed the
            // arm was "stored on the template", which it was not, so an account
            // minted after the request paid the unmodified fee.
            run.arm(
                None,
                VenueArm::FeeSurcharge {
                    mult,
                    armed_ns: now_ns(),
                    span_ns: sim_duration_from_millis(window_ms),
                },
            )
            .await;
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
            // SCOPED BY THE REQUEST'S `account` WHEN IT NAMES ONE, exactly like
            // every other arm on this plane. Client order ids are unique within
            // a trader's own book and not across the venue's, so an unqualified
            // search on a multi-account exchange can silently cancel a
            // stranger's resting order; naming the account is how a scenario
            // driving fifty subagents says which book it meant.
            let Some((holder, order_symbol)) = run.account_holding(target, &client_order_id).await
            else {
                // No account rests this id. Let a ledger say WHY - unknown id
                // and already-terminal are different diagnoses, and this arm
                // must not flatten them into one message.
                //
                // OFF THE LEDGER THE REQUEST TARGETED, which is the same scope
                // the search above used. Diagnosing off the DEFAULT account
                // whatever the request named - which is what this did - asks
                // the wrong book, and the answer it gives is about an account
                // the operator did not name.
                //
                // AND WITH A QUERY, NEVER THE CANCEL. This ran
                // `cancel_open_order_silently` for its `Err`, which is not a
                // read: on `Ok` it closes the order out and reaps its held
                // children. So a request naming an account that does NOT hold
                // the id, on a venue where the default account DOES, silently
                // cancelled the default's order and answered `404 unknown
                // order` because the `Err` was `None` - the exact cross-account
                // cancel the scoping above closes. `silent_cancel_refusal` is
                // the non-mutating form and shares its wording with the cancel.
                //
                // A ledger that does not exist is not built for the sentence:
                // an unopened ledger holds nothing, so its only possible answer
                // is the "unknown order" default below, and constructing one to
                // hear it back was dead by construction.
                let diagnosed = match target {
                    Some(named) => mogwai_protocol::AccountId::parse(named).ok(),
                    None => Some(run.default_account_id()),
                };
                let reason = match diagnosed.as_ref().and_then(|id| run.peek_account(id)) {
                    Some(ledger) => ledger
                        .engine
                        .lock()
                        .await
                        .silent_cancel_refusal(&client_order_id),
                    None => None,
                }
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
            let ts = state.river_now(&order_symbol, None).await.ns;
            if let Err(reason) = holder
                .engine
                .lock()
                .await
                .cancel_open_order_silently(&client_order_id, ts)
            {
                tracing::warn!(%client_order_id, reason, "refusing silent cancel");
                return (StatusCode::NOT_FOUND, reason);
            }
            tracing::info!(%client_order_id, "silently canceled resting order venue-side");
        }
        Divergence::ClearDivergences => {
            // Lift both venue-owned temporal windows. `None` is the cleared
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
                                "river {symbol} has a placed boat; generator controls cannot mutate live water"
                            ),
                        );
                    }
                    vec![symbol.to_owned()]
                }
                // Unqualified, this clears every river whose water exists and
                // carries no boat. A boated river is SKIPPED rather than
                // refused: the transport half of this control is run-wide and
                // must stay reachable while a boat is sitting.
                None => state
                    .rivers
                    .materialized_symbols()
                    .into_iter()
                    .filter(|symbol| run.boatyard.boat_for_symbol(symbol).is_none())
                    .collect(),
            };
            // EVERY account's transport arms and fee surcharge, whatever the
            // request named: a clear is an operator saying "stop everything",
            // and clearing only one account's would leave a blackout armed
            // somewhere the request never mentioned. THE VENUE RECORD IS
            // CLEARED WITH THEM, or an account connecting after a clear would be
            // opened from a record the operator already lifted.
            //
            // That covers all six `CommandLatency` fields too. It clears what
            // the venue will do to commands it has NOT started acting on, and
            // lifts an ack window off frames already queued (the pump reads that
            // one per event at dequeue). It does NOT reach into an act delay
            // already being served: that command's sleep was read once, at
            // detach, and a venue that has begun acting does not un-begin.
            run.clear_venue_arms().await;
            for symbol in clearing {
                state.rivers.clear_flow_surge(&symbol);
            }
        }
        // Venue-ownership contract (pins B.4 / E.11): the EIGHT variants the
        // arms above intercept - `DelayAcks`, `CommandLatency`, `GoDark`,
        // `StallData`, `FlowSurge`, `FeeSurcharge`, `ClearDivergences` and
        // `CancelOpenOrderSilently` - are venue-owned controls with no
        // synchronous engine-side trigger. The venue owns them and must NEVER
        // forward them to `engine.arm()`, which would drop them on the floor.
        //
        // THIS ARM IS ENUMERATED RATHER THAN A CATCH-ALL, and that is the whole
        // guard: this is the ROUTING site, so a misclassification here loses a
        // user-visible control silently. A catch-all guarded by a
        // `debug_assert!` cannot carry that, because the release profile the
        // socket suites run in compiles the assert out entirely. With every
        // variant named, a new `Divergence` fails to BUILD here until someone
        // routes it, in both profiles and on every toolchain.
        //
        // The five names below are the same engine-armed set `Engine::arm`
        // enumerates in `mogwai-engine/src/divergence.rs`; that match is
        // exhaustive too, so a variant classified venue-owned there and
        // forwarded here would fail this crate's build rather than being
        // queued as a dead entry.
        // TERMINAL, so it is handled before the engine-armed set and never
        // recorded: there is no later ledger for a venue arm to replay onto, and
        // no `ClearDivergences` that could reach it.
        Divergence::FaultTape => {
            // The account scope is REFUSED rather than ignored. A venue fault is
            // the whole process going away, so naming one account reads as a
            // request nothing here can honour, and silently widening it to the
            // venue is how an operator ends up killing a run they meant to
            // perturb one ledger of.
            if request.account.is_some() {
                return (
                    StatusCode::BAD_REQUEST,
                    "FaultTape takes down the whole venue and cannot be scoped to one account"
                        .to_string(),
                );
            }
            if !run.fault_venue() {
                // Not an error. The receiver is gone, which means the venue is
                // already tearing down - the state the arm was asking for.
                tracing::info!("FaultTape arrived while the venue was already ending");
                return (
                    StatusCode::ACCEPTED,
                    "the venue was already tearing down when this arrived".to_string(),
                );
            }
            tracing::warn!("FaultTape armed: the venue will report a fault and exit nonzero");
            return (
                StatusCode::ACCEPTED,
                "the venue is faulting and will exit nonzero".to_string(),
            );
        }
        engine_div @ (Divergence::PartialFillNext { .. }
        | Divergence::RejectNextSubmit { .. }
        | Divergence::RejectNextCancel { .. }
        | Divergence::DuplicateNextFill
        | Divergence::DropNextAccountUpdate) => {
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
            //
            // VENUE-WIDE WHATEVER THE REQUEST NAMED, hence the `None` rather
            // than `target`: an engine divergence is a statement about the
            // venue's matching, and the wire has never routed one to a single
            // account. It reaches every ledger AND THE RUN, so a ledger minted
            // later opens holding it. Walking only the ledgers that already
            // exist - which is what this did - meant an operator could arm a
            // `PartialFillNext`,
            // start a subagent, and get a run that believed it was perturbed and
            // was not.
            //
            // The eviction report is whichever ledger shed one. That reads as
            // representative because every ledger holds the same arms and hits
            // the cap together, which is a claim `Run::arm` makes true rather
            // than assumes: the run's own record sheds from the oldest end on
            // the same cap, so a ledger opened mid-run replays the queue an
            // older one is holding.
            let evicted = run.arm(None, VenueArm::Engine(engine_div.clone())).await;
            if let Some(shed) = evicted {
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
    /// What the venue is enforcing against this account right now. A sibling
    /// rather than part of `AccountState`, because that type is also the PUSHED
    /// frame's payload and risk state is an evaluator's concern rather than
    /// something every fill should carry.
    risk: mogwai_protocol::risk::RiskState,
}

/// Which axis a timestamp lives on. The sibling of `VenueClock::boat_clock`,
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
/// WHOSE ACCOUNT is named by `?account=`, defaulting to the venue's default
/// account - the same resolution the socket does, so a consumer that named no
/// account on either surface sees one ledger. An id nobody has traded under is
/// not an error: the answer is the opening balances a ledger under that id
/// WOULD carry, which is what a consumer asking before its first order expects.
///
/// A READ DOES NOT ALLOCATE, and it used to. This resolved through
/// `Run::account`, which is create-on-first-sight, so an unauthenticated GET
/// minted a ledger for any id in the query string - born frozen and collectable
/// only when `account_ttl_ms > 0`, and the default is to keep accounts forever.
/// The answer is unchanged, because `Run::unopened_ledger` builds exactly what
/// the mint would have and then throws it away; what changed is that asking
/// about an account is no longer the same act as opening one.
///
/// STAMPED ON THE VENUE CLOCK, deliberately. A ledger spans every river its
/// account's passengers have boarded, so there is no boat axis to put it on: stamp from one
/// boat and a push from a later-placed boat on another river is AHEAD of the
/// pull; stamp from the newest and it is behind. No choice can keep a
/// cross-clock monotonicity promise, so the answer keeps the venue stamp and
/// says so, and a consumer orders pulls against pushes BY SEQUENCE.
pub(crate) async fn account(
    Query(query): Query<AccountQuery>,
    State(state): State<AppState>,
) -> Response {
    let account_id = match &query.account {
        Some(named) => match mogwai_protocol::AccountId::parse(named) {
            Ok(account_id) => account_id,
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    format!("account id is not usable: {error}"),
                )
                    .into_response();
            }
        },
        None => state.run.default_account_id(),
    };
    let ts = sim_now_ns(state.venue_sim());
    let (account, risk) = match state.run.peek_account(&account_id) {
        Some(account_state) => {
            let mut engine = account_state.engine.lock().await;
            let account = engine.account_snapshot(ts);
            // Published for the EVALUATOR, not for the strategy. A real trader
            // reads its remaining drawdown off the firm's dashboard; mogwai
            // presents none, so a run that ended flat having spent 90 percent
            // of its budget would be indistinguishable from one that never came
            // close.
            let ledger = account_state
                .risk
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let risk = risk_state(&engine, &ledger, &account);
            drop(ledger);
            (account, risk)
        }
        None => {
            let (mut engine, ledger) = state.run.unopened_ledger(&account_id);
            let account = engine.account_snapshot(ts);
            let risk = risk_state(&engine, &ledger, &account);
            (account, risk)
        }
    };
    Json(AccountSnapshot {
        clock: ClockAxis::Venue,
        account,
        risk,
    })
    .into_response()
}

/// The risk half of an account snapshot, over whichever ledger produced the
/// account half, an existing account's or a preview of an unopened one.
///
/// An unpoliced account still reports its equity, which is the one number an
/// evaluator wants whether or not anything is enforced against it. With no
/// policy currency there is nothing to compute it in, so it reports the
/// settlement currency's balance only when exactly one is held.
fn risk_state(
    engine: &mogwai_engine::Engine,
    ledger: &crate::risk::RiskLedger,
    account: &AccountState,
) -> mogwai_protocol::risk::RiskState {
    let currency = ledger
        .currency()
        .map(str::to_owned)
        .or_else(|| sole_currency(account));
    let equity = currency
        .and_then(|currency| crate::risk::equity_in(engine, &currency))
        .unwrap_or_default();
    ledger.state(equity)
}

#[derive(Default, Deserialize)]
pub(crate) struct AccountQuery {
    #[serde(default)]
    account: Option<String>,
}

/// Whether an account measured in `currency` can state what trading this shape
/// leaves it holding.
///
/// A FUTURE settling in `currency` moves only that currency and carries its own
/// unrealized, so it always qualifies. A SPOT pair leaves the base asset in the
/// ledger as a balance, which is valuable exactly when this pair QUOTES it in
/// `currency` - that pair's own mark is the price. Anything else would leave a
/// holding nothing prices.
fn settles_only_in(def: &InstrumentDef, currency: &str) -> bool {
    def.class.settlement_currency() == currency
}

/// The one currency an account holds, or `None` if it holds none or several.
/// Used only to report an UNPOLICED account's equity, where no policy names a
/// currency to compute it in; a policed account always has one.
fn sole_currency(account: &AccountState) -> Option<String> {
    let mut held = account
        .balances
        .iter()
        .filter(|balance| !balance.total.is_zero());
    let first = held.next()?;
    // Several, and nothing says which of them the account is measured in.
    held.next().is_none().then(|| first.currency.clone())
}

/// Open an account on terms the consumer states, before it trades.
///
/// Structured account config goes over HTTP for the same reason a divergence
/// does: it is a nested document validated at its own boundary, and the socket
/// query string carries scalars. A socket then names the account it opened with
/// `?account=`, and only that id crosses the upgrade.
///
/// OPTIONAL, and that is the design rather than a convenience. Account
/// resolution is TOTAL: a connection that never calls this is served under the
/// default account, so the ephemeral single-consumer venue needs no call at all.
/// What this buys is the case the default cannot express - a batch of subagents
/// on one exchange, each sized differently.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OpenAccountRequest {
    account_id: String,
    /// Opening balances by currency. The venue's `[balances]` is what an
    /// unnamed account gets; this is the same value stated per account, which
    /// is what makes a 25k experiment and a 100k experiment runnable on one
    /// venue.
    ///
    /// STRING-SPELLED, like every other money quantity that crosses into the
    /// venue: `{"USDT":"250000"}`, never `{"USDT":250000}`. A bare JSON number
    /// goes through `f64`, so a wide opening balance would be silently rounded
    /// and `1e-30` would fund the account with zero. This is a live decode path
    /// the wire-decimal round nearly missed - it is not in `messages` and does
    /// not look like a frame - and it is pinned by
    /// `an_opening_balance_must_be_spelled_as_a_string`. The policy fields
    /// BELOW stay number-tolerant on purpose: they are thresholds and fractions
    /// that are also spelled in TOML.
    #[serde(with = "mogwai_protocol::decimal::str_map")]
    balances: std::collections::HashMap<String, rust_decimal::Decimal>,
    /// The rules the venue ENFORCES against this account, stated inline.
    /// Absent means unpoliced unless `policy_preset` names one.
    #[serde(default)]
    policy: mogwai_protocol::risk::AccountPolicy,
    /// A registered or shipped policy to use instead of restating one.
    ///
    /// Resolution is total and three-step, the same shape a symbol resolves in:
    /// inline knobs win, else this name, else unpoliced. A name NOBODY has is an
    /// error rather than a silent fall to unpoliced, because a run that believes
    /// it is enforced and is not is the worst of the three outcomes.
    #[serde(default)]
    policy_preset: Option<String>,
}

pub(crate) async fn open_account(
    State(state): State<AppState>,
    Json(request): Json<OpenAccountRequest>,
) -> impl IntoResponse {
    let account_id = match mogwai_protocol::AccountId::parse(&request.account_id) {
        Ok(account_id) => account_id,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("account id is not usable: {error}"),
            );
        }
    };
    if request.balances.is_empty() {
        // An account funded in nothing can hold no position and would meet a
        // funds rejection on its first order, which reads as depletion. Naming
        // it here keeps a configuration mistake distinguishable from a trading
        // outcome, which is the whole reason the two refusals are kept apart.
        return (
            StatusCode::BAD_REQUEST,
            "an account must open with at least one funded currency".to_owned(),
        );
    }
    let policy = match state
        .run
        .resolve_policy(request.policy_preset.as_deref(), request.policy)
    {
        Ok(policy) => policy,
        Err(refusal) => return (StatusCode::BAD_REQUEST, refusal.to_string()),
    };
    // Validated where the policy ENTERS the venue, so a nonsense rule is a
    // refused request rather than an account that behaves strangely hours in.
    // After resolution, so a shipped preset is held to the same bar as an inline
    // policy rather than trusted for being ours.
    if let Err(error) = policy.validate() {
        return (StatusCode::BAD_REQUEST, error);
    }
    let policed = !policy.is_unpoliced();
    match state
        .run
        .open_account(&account_id, request.balances, policy)
    {
        Ok(()) => {
            tracing::info!(account = %account_id.as_str(), policed, "opened an account");
            (StatusCode::CREATED, String::new())
        }
        Err(refusal) => (StatusCode::CONFLICT, refusal.to_string()),
    }
}

#[derive(Default, Deserialize)]
pub(crate) struct ClockQuery {
    #[serde(default)]
    symbol: Option<String>,
    #[serde(default)]
    speed: Option<f64>,
}

pub(crate) async fn clock(
    Query(query): Query<ClockQuery>,
    State(state): State<AppState>,
) -> Json<VenueClock> {
    // Publish the tape boundary alongside the affine map so a consumer can guard
    // its own warmup against `data_origin_ns` rather than issuing a doomed
    // off-tape fetch. `venue_now_ns` is sampled here so the consumer gets sim-now
    // and the floor from one round trip, without reading its own (skewable) wall
    // clock. `data_origin_ns` is the fixed floor; the horizon is echoed so
    // the consumer can report the floor in its own terms.
    //
    // `?symbol=` answers for that river's lead boat when more than one
    // cadence is placed; `?speed=` names one.
    // For a boat, `venue_now_ns` is the sim instant of the last tick it
    // PUBLISHED rather than the affine map evaluated at the wall: a boat placed
    // mid-run is deliberately behind its own clock's projection, and a consumer
    // pacing against a projection the feed has not reached would ask for water
    // that has not been delivered. With no boat the two coincide.
    let now = if let Some(symbol) = query.symbol.as_deref() {
        state.river_now(symbol, query.speed).await
    } else {
        RiverNow::venue(state.venue_sim())
    };
    Json(VenueClock {
        sim: now.sim,
        venue_now_ns: now.ns,
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
    msg: Command,
    state: &AppState,
    boat: &Arc<crate::boatyard::Boat>,
    ts: u64,
    socket_symbol: &mogwai_protocol::Symbol,
) -> (Command, Option<mogwai_engine::MarketReading>) {
    // Whether this command needs a reading at all. WHICH river it is read for
    // is never in question: a submit's wire symbol was already refused unless
    // it equals the socket's, an amend names no symbol at all, and the socket's
    // symbol is the one its boat was boarded on. Both the memoized walk and the
    // `read_last` fallback below therefore take the boat's own river, so the
    // two cannot name different rivers even if that resolution ever changes.
    let needs_reading = match &msg {
        Command::SubmitOrder(_) | Command::SubmitOrderGroup { .. } => {
            for order in submitted_orders(&msg) {
                debug_assert_eq!(order.symbol.as_ref(), boat.symbol());
            }
            true
        }
        // An amend names no symbol on the wire, so it resolves to the river
        // this socket is bound to.
        Command::ModifyOrder { price: Some(_), .. } => {
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
        // EVERY member of a group is stamped, off the SAME reading. A bracket
        // whose market entry took one price while a sibling took another would
        // have met two markets in one atomic admission, which is the property
        // the group frame exists to guarantee against.
        let stamp = |order: &mut mogwai_protocol::SubmitOrder| {
            if order.order_type == OrderType::Market && order.price.is_none() {
                order.price = last_px;
            }
        };
        let msg = match msg {
            Command::SubmitOrder(mut order) => {
                stamp(&mut order);
                Command::SubmitOrder(order)
            }
            Command::SubmitOrderGroup { mut orders } => {
                orders.iter_mut().for_each(stamp);
                Command::SubmitOrderGroup { orders }
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
    let history_slot =
        acquire_history_slot(&state.history_slots, &state.history_slot_waiters).await?;
    // No `regime` parameter: the market regime is boot config for the whole run
    // now, so history and the live tape are the same realization by
    // construction rather than by a consumer remembering to ask for the same one.
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
    let river_now = state.river_now(&symbol, None).await.ns;
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
    // permanently and by construction. So a consumer that reads `/clock` with no
    // `?symbol=` and passes that instant as `end` is routinely far ahead of
    // this answer, and refusing it would fail every honest warmup fetch to
    // prevent nothing: the clamp serves exactly the data that exists and no
    // more, so nothing is silently short about it. A consumer that needs to know
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
    // history slot RIDES that task rather than staying a handler local: hyper
    // drops this handler future the moment the connection drops, but a running
    // blocking task cannot be cancelled, so a slot left out here would be
    // released while the synthesis it covers was still resident -
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
    let history_slot =
        acquire_history_slot(&state.history_slots, &state.history_slot_waiters).await?;
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
    let river_now = state.river_now(&symbol, None).await.ns;
    if let Some(body) = history_start_refusal(start, data_origin, river_now) {
        return Err((StatusCode::BAD_REQUEST, body));
    }
    // A future start is impossible, while a future end is the ordinary
    // caller-clock spelling of "everything through now", so clamp the latter.
    let end = Some(end.map_or(river_now, |end| end.min(river_now)));
    // The permit rides the blocking task for the same reason as `/trades`: a
    // a dropped connection drops this future, and only the closure outlives that.
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
/// request WAITS for a slot rather than starting one - see
/// [`HISTORY_SLOT_WAIT`] - so history can neither fill Tokio's blocking pool
/// ahead of order-entry market readings nor accumulate response buffers
/// without a ceiling.
///
/// The memory bound this buys is MEASURED rather than asserted, by
/// `worst_case_history_page_bytes` and recorded in `reference/performance.md`:
/// a full `/quotes` page is 4.40 MB of `QuoteTick` vector and 5.90 MB of JSON
/// resident together while it serializes, so four of them peak near 41 MB.
/// `/trades` is narrower at 3.20 MB plus 5.05 MB. The number is a bound only
/// because the permit outlives BOTH halves, which is what `HistoryPage` is
/// for.
pub(crate) const MAX_CONCURRENT_HISTORY_SLOTS: usize = 4;

/// A serialized history page that owns its history slot.
///
/// The permit cannot simply be a handler local. Axum serializes a returned
/// `Json` value AFTER the handler future resolves, so a permit dropped at the
/// end of the handler is released while multi-megabyte responses are still
/// being built - four completed syntheses would free four slots while their
/// bytes were still resident, and the ceiling above would bound nothing. So
/// serialization happens on the synthesis's own blocking task and the permit
/// travels with the finished bytes: it is released when hyper drops this body,
/// which is after the response has been written.
///
/// The permit's OTHER end is guarded too: it is moved INTO the blocking
/// closure, not merely awaited past, because a dropped connection drops the
/// handler future while the blocking task keeps running to completion. A
/// permit held by the dropped future would free its slot for a new synthesis
/// while the orphaned one was still resident; one held by the closure returns
/// only when the work it covers actually ends.
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
    // raw-socket test consumer does not implement. The length is known exactly
    // here (the whole page is already serialized), so state it and keep the
    // response byte-shaped exactly as the `Json` it replaced.
    headers.insert(
        axum::http::header::CONTENT_LENGTH,
        axum::http::HeaderValue::from(length),
    );
    response
}

/// How long a history request WAITS for a slot before the venue refuses it.
///
/// The gate was fail-fast, and that was wrong for the topology this venue is
/// for. The cap exists to bound RESIDENT MEMORY - four multi-megabyte pages -
/// and a request that is merely waiting holds no page, so refusing it bought
/// the memory bound nothing and cost the consumer everything: nautilus's
/// historical response types carry no error channel, so an adapter's only
/// alternative to an unresolvable hang is to resolve the request EMPTY and log
/// why. A refused warmup therefore reaches the consumer as a QUIET WINDOW,
/// indistinguishable from a tape that genuinely printed nothing, and the run
/// then reasons about a market it was never shown.
///
/// That was survivable when one consumer owned one venue. It is not survivable in
/// the attach topology, which exists to point tens of runs at one venue: one
/// warmup is not one request, because the venue serves no bars and the adapter
/// pages `/trades` and aggregates locally, so a boot storm is dozens of runs
/// each taking dozens of sequential pages against four slots. Ordinary paging
/// would fire the gate constantly, and silently.
///
/// WAITING FIXES THAT WITHOUT WEAKENING THE BOUND. Four syntheses are resident
/// at once whatever the queue does, so the measured ~41 MB ceiling is untouched;
/// what changes is that the fifth caller is SERVED LATE instead of told nothing
/// happened. The deadline is what keeps "late" from becoming "never": a consumer
/// that waits this long is looking at a venue that is genuinely saturated rather
/// than merely busy, and a refusal it can see beats a hang it cannot.
///
/// Generous on purpose. A full page synthesis is the dominant cost, so the wait
/// has to cover several of them ahead in the queue; sizing it to one would
/// reintroduce the refusal for exactly the paging this exists to absorb.
pub(crate) const HISTORY_SLOT_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// How many history requests may be QUEUED for a slot at once.
///
/// The wait above needs its own bound or it is not a bound at all: an unbounded
/// queue turns a saturated venue into one that accepts everything and answers
/// nothing, holding a connection and a task per waiter. This is what stays
/// fail-fast, and it is the refusal an operator should read as real overload
/// rather than as ordinary contention. Sized well above the concurrency so a
/// mass-attach boot storm queues rather than trips it, and far below anything
/// that could exhaust the listener.
pub(crate) const MAX_QUEUED_HISTORY_REQUESTS: usize = 128;

/// The one slot decision both history endpoints make, so the cap and its
/// refusal cannot drift apart between `/trades` and `/quotes`.
///
/// TWO GATES, and they answer different questions. `queue` is fail-fast and asks
/// whether the venue is genuinely overloaded; `gate` is a bounded WAIT and asks
/// only whether a slot is free yet. See [`HISTORY_SLOT_WAIT`] for why the
/// second is a wait rather than a refusal.
///
/// The queue permit is dropped as soon as a slot is won: it counts callers
/// still QUEUEING, and a caller holding a slot has stopped. The returned
/// permit is the SLOT, which travels with the finished bytes.
async fn acquire_history_slot(
    gate: &Arc<tokio::sync::Semaphore>,
    queue: &Arc<tokio::sync::Semaphore>,
) -> Result<tokio::sync::OwnedSemaphorePermit, (StatusCode, String)> {
    let queued = Arc::clone(queue).try_acquire_owned().map_err(|_| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "history request capacity exhausted".to_owned(),
        )
    })?;
    let slot = tokio::time::timeout(HISTORY_SLOT_WAIT, Arc::clone(gate).acquire_owned())
        .await
        .map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "history request capacity exhausted".to_owned(),
            )
        })?
        // The semaphore is never closed - it lives as long as the process - so
        // the only `acquire_owned` error is closure, and reaching it would mean
        // the venue is already tearing down.
        .map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "history request capacity exhausted".to_owned(),
            )
        })?;
    drop(queued);
    Ok(slot)
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
/// Echo the request back TRUNCATED, for the reason `truncate_echoed_id` exists
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
/// venue-side: `axum::extract::Query` percent-decodes before serde sees the
/// field, so `?symbol=%4DNQ` arrives as `MNQ` and passes, exactly as it should.
/// The needs-no-encoding framing belongs to the consumer-side caller of
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
/// `river_now` is the named river's ceiling, not the venue's: for a boated
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
mod health_fault_tests {
    use super::*;
    use mogwai_data::{ArrivalRefusal, TickFault};

    fn refusal(clock_ns: u64) -> TickFault {
        TickFault::Arrival(ArrivalRefusal::NonFiniteState { clock_ns })
    }

    /// A fault on a NON-BOOT river reaches the poll. The handler used to read
    /// one boat - the boot river's - so a consumer bound to any other river got a
    /// healthy answer while its own tape was stuck, and a fire-and-forget run
    /// was scored keepable on the strength of a river nobody was using.
    #[test]
    fn a_fault_on_any_boated_river_is_reported() {
        let reported = health_fault([("NOT-THE-BOOT-RIVER".to_owned(), refusal(77))])
            .expect("a faulted river is reported");
        assert_eq!(reported.symbol, "NOT-THE-BOOT-RIVER");
        assert_eq!(reported.kind, "arrival.non_finite_state");
        assert_eq!(reported.clock_ns, 77);
        assert!(health_fault([]).is_none(), "an unfaulted run reports none");
    }

    /// One optional object, N boats: which one answers may not depend on the
    /// order the registry happened to iterate, or a fleet poller watching a
    /// faulted run sees the field flicker between rivers across polls.
    #[test]
    fn the_reported_river_does_not_depend_on_iteration_order() {
        let faults = [
            ("MNQ".to_owned(), refusal(2)),
            ("BTCUSDT".to_owned(), refusal(1)),
            ("MES".to_owned(), refusal(3)),
        ];
        let forward = health_fault(faults.clone()).expect("faulted");
        let mut reversed = faults;
        reversed.reverse();
        let backward = health_fault(reversed).expect("faulted");
        assert_eq!(forward.symbol, "BTCUSDT");
        assert_eq!(backward.symbol, forward.symbol);
        assert_eq!(backward.clock_ns, forward.clock_ns);
    }
}

#[cfg(test)]
mod history_slot_tests {
    use super::*;

    fn gates() -> (Arc<tokio::sync::Semaphore>, Arc<tokio::sync::Semaphore>) {
        (
            Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_HISTORY_SLOTS)),
            Arc::new(tokio::sync::Semaphore::new(MAX_QUEUED_HISTORY_REQUESTS)),
        )
    }

    /// THE FIFTH REQUEST WAITS, and this is the whole point of the change.
    ///
    /// It used to be refused, and a refusal reaches the consumer as an EMPTY
    /// window rather than as an error - nautilus's historical response types
    /// carry no error channel - so a run read a refused warmup as a market that
    /// printed nothing. The cap bounds RESIDENT pages, and a waiter holds no
    /// page, so waiting costs the bound nothing.
    ///
    /// Drives the endpoints' own slot decision rather than a semaphore of its
    /// own: `acquire_history_slot` is what both handlers call. Disclosed
    /// residual - this pins the decision, not that a handler still makes it;
    /// the handlers call it on their first line, and no in-crate test can build an
    /// `AppState` cheaply enough to prove the call site.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn a_history_request_over_the_cap_waits_for_a_slot_rather_than_being_refused() {
        assert_eq!(MAX_CONCURRENT_HISTORY_SLOTS, 4);
        let (gate, queue) = gates();
        let held = futures_util::future::join_all(
            (0..MAX_CONCURRENT_HISTORY_SLOTS).map(|_| acquire_history_slot(&gate, &queue)),
        )
        .await
        .into_iter()
        .map(|slot| slot.expect("slot"))
        .collect::<Vec<_>>();

        // The fifth is still waiting after a span that would have covered any
        // number of immediate refusals.
        let mut fifth = Box::pin(acquire_history_slot(&gate, &queue));
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), &mut fifth)
                .await
                .is_err(),
            "the fifth request waits rather than being answered"
        );

        // And it is SERVED when a slot returns, which is what the consumer gets
        // instead of a silently empty window.
        drop(held);
        assert!(fifth.await.is_ok(), "a returned slot wakes the waiter");
    }

    /// The wait is bounded, or it is not a bound. A venue that never frees a
    /// slot answers with the refusal rather than holding the caller forever: a
    /// refusal it can see beats a hang it cannot.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn a_wait_that_outlives_its_deadline_is_still_refused() {
        let (gate, queue) = gates();
        let _held = futures_util::future::join_all(
            (0..MAX_CONCURRENT_HISTORY_SLOTS).map(|_| acquire_history_slot(&gate, &queue)),
        )
        .await
        .into_iter()
        .map(|slot| slot.expect("slot"))
        .collect::<Vec<_>>();

        let (status, reason) = acquire_history_slot(&gate, &queue)
            .await
            .expect_err("the deadline expires");
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(reason, "history request capacity exhausted");
    }

    /// The queue has its own bound, and it is the one that still fails FAST. An
    /// unbounded queue would turn a saturated venue into one that accepts
    /// everything and answers nothing, holding a task per waiter.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn the_queue_itself_is_bounded_and_refuses_immediately() {
        let (gate, queue) = gates();
        // Every queue permit taken and none of them released, which is the
        // saturated venue this refusal is for.
        let _queued = (0..MAX_QUEUED_HISTORY_REQUESTS)
            .map(|_| {
                Arc::clone(&queue)
                    .try_acquire_owned()
                    .expect("a queue slot")
            })
            .collect::<Vec<_>>();

        let refused = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            acquire_history_slot(&gate, &queue),
        )
        .await
        .expect("the queue refusal does not wait out the slot deadline");
        assert_eq!(
            refused.expect_err("refused").0,
            StatusCode::SERVICE_UNAVAILABLE
        );
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

    /// The worst-case resident cost of one page holding a slot, which is what
    /// `MAX_CONCURRENT_HISTORY_SLOTS` multiplies. `/quotes` is the wider
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
    fn history_slot_overhead() {
        const ITERATIONS: usize = 1_000_000;
        let gate = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_HISTORY_SLOTS));
        let started = std::time::Instant::now();
        for _ in 0..ITERATIONS {
            drop(Arc::clone(&gate).try_acquire_owned().expect("slot"));
        }
        let elapsed = started.elapsed();
        eprintln!(
            "iterations={ITERATIONS} elapsed_ns={} ns_per_acquire={}",
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

/// A PAGE MAY BE CUT MID-INSTANT, and that is safe ONLY because the generator
/// never prints two trades at one instant. The rule a consumer is held to -
/// `AGENTS.md`'s frontier family: a timestamp-only cursor may advance onto an
/// instant only once every row at that instant has been seen - would be
/// unsatisfiable against this surface if ties existed, since the venue ships no
/// opaque cursor and resuming at `last_ts` repeats while `last_ts + 1` drops.
/// broadarrow filed that hazard on 2026-08-18; it was measured, found
/// unreachable, and pinned by `mogwai-data`'s
/// `a_river_never_prints_two_trades_at_one_instant`. Children are stamped at a
/// 1 us stride and the arrival kernel floors a parent's advance at the same
/// stride, so one river's trades are strictly increasing. A quote ties with its
/// FIRST CHILD only, and `/trades` and `/quotes` are separate pages, so no tie
/// can cut either. Break that generator property and this surface is wrong; the
/// `mogwai-data` test is what says so.
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

    /// `POST /accounts` IS THE THIRD LIVE DECODE PATH CARRYING MONEY INTO THE
    /// VENUE, after the execution wire and the market-data wire, and it was
    /// missed when those were annotated - it is not a frame, it does not live
    /// in `messages`, and it looks like config while behaving like a request.
    ///
    /// An opening balance decoded through `f64` would be silently rounded at
    /// width and `1e-30` would fund the account with nothing, which is the same
    /// hazard the wire fields carry. Every in-tree caller already spells these
    /// as strings, so refusing numbers breaks no known peer.
    ///
    /// THE POLICY FIELDS BESIDE IT ARE DELIBERATELY STILL TOLERANT, and the
    /// second half of this test says so: they are thresholds and fractions that
    /// are also spelled in TOML, so `max_position = 5` is their natural form.
    #[test]
    fn an_opening_balance_must_be_spelled_as_a_string() {
        let string_spelled = r#"{"account_id":"WYRD-900","balances":{"USDT":"250000.75"}}"#;
        let request: OpenAccountRequest =
            serde_json::from_str(string_spelled).expect("the string spelling must decode");
        assert_eq!(
            request.balances.get("USDT").copied(),
            Some(rust_decimal::Decimal::new(25_000_075, 2))
        );

        for numeric in [
            r#"{"account_id":"WYRD-900","balances":{"USDT":250000.75}}"#,
            r#"{"account_id":"WYRD-900","balances":{"USDT":1e-30}}"#,
        ] {
            assert!(
                serde_json::from_str::<OpenAccountRequest>(numeric).is_err(),
                "a numeric opening balance must be refused: {numeric}"
            );
        }

        // The deliberate exception, asserted rather than described: a policy
        // threshold in the same body still takes a bare number.
        let policy_numeric = r#"{"account_id":"WYRD-901","balances":{"USDT":"1000"},"policy":{"currency":"USDT","max_position":{"quantity":5}}}"#;
        let request: OpenAccountRequest =
            serde_json::from_str(policy_numeric).expect("a numeric policy threshold stays legal");
        assert_eq!(
            request.policy.max_position.map(|cap| cap.quantity),
            Some(rust_decimal::Decimal::from(5))
        );
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
            trail_offset: None,
            limit_offset: None,
            reduce_only: false,
            post_only: false,
            time_in_force: TimeInForce::Gtc,
            expire_time: None,
            link: None,
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

        // A MARKET-TO-LIMIT TAKES THE MARKET, so it owes the same refusal as the
        // marketable limit above. The two cases run the IDENTICAL order but for
        // the stated price, so the refusal cannot be coming from the type alone:
        // the non-marketable one must still be admitted, exactly as its limit
        // sibling is.
        let mut mtl_resting = resting.clone();
        mtl_resting.order_type = OrderType::MarketToLimit;
        assert!(
            !reject_while_closed(&calendar, closed, &mtl_resting, reading),
            "a market-to-limit short of the stale print rests through a closure"
        );
        let mut mtl_marketable = mtl_resting.clone();
        mtl_marketable.price = Some(Decimal::from(30_000));
        assert!(
            reject_while_closed(&calendar, closed, &mtl_marketable, reading),
            "a marketable market-to-limit would fill off the stale print and must be refused"
        );
    }

    fn linked_leg(id: &str, siblings: &[&str]) -> SubmitOrder {
        SubmitOrder {
            client_order_id: id.into(),
            symbol: "MNQ".into(),
            position_id: None,
            side: Side::Buy,
            order_type: OrderType::Limit,
            quantity: Decimal::ONE,
            price: Some(Decimal::from(100)),
            trigger_price: None,
            trail_offset: None,
            limit_offset: None,
            reduce_only: false,
            post_only: false,
            time_in_force: TimeInForce::Gtc,
            expire_time: None,
            link: Some(mogwai_protocol::OrderLink {
                order_list_id: "OL-1".into(),
                contingency: mogwai_protocol::Contingency::Ouo,
                linked_order_ids: siblings.iter().map(|id| (*id).to_string()).collect(),
                parent_order_id: None,
            }),
        }
    }

    /// THE WIRE RULE that makes the group frame's guarantee worth anything: a
    /// linked order sent ALONE is refused, because the per-leg route is what
    /// lets leg one fill before leg two is admitted.
    #[test]
    fn a_linked_order_may_not_be_submitted_alone() {
        let reason = boundary_error(&Command::SubmitOrder(linked_leg("A", &["B"])))
            .expect("a linked bare submit is refused");
        assert!(reason.contains("SubmitOrderGroup"), "{reason}");

        // The negative, without which the assertion above holds for a boundary
        // that refuses every submit: an UNLINKED order still goes alone, which
        // is every order this venue served before linkage existed.
        let mut standalone = linked_leg("A", &[]);
        standalone.link = None;
        assert!(boundary_error(&Command::SubmitOrder(standalone)).is_none());
    }

    /// A group is self-contained, and the boundary is where that is enforced:
    /// a member naming an outsider could not be promised that admitting the
    /// group admits every sibling.
    #[test]
    fn a_group_may_not_link_outside_itself() {
        let group = Command::SubmitOrderGroup {
            orders: vec![linked_leg("A", &["B"]), linked_leg("B", &["OUTSIDER"])],
        };
        assert!(boundary_error(&group).is_some_and(|reason| reason.contains("own members")));

        let legal = Command::SubmitOrderGroup {
            orders: vec![linked_leg("A", &["B"]), linked_leg("B", &["A"])],
        };
        assert!(
            boundary_error(&legal).is_none(),
            "a self-contained pair is admitted"
        );
    }

    /// A refused group answers EVERY member. One frame naming one member would
    /// leave the consumer waiting on the rest of a bracket the venue has already
    /// refused whole.
    #[test]
    fn a_refused_group_answers_every_member() {
        let group = Command::SubmitOrderGroup {
            orders: vec![linked_leg("A", &["B"]), linked_leg("B", &["OUTSIDER"])],
        };
        let frames = boundary_refusal(&group, "no", 7);
        let ids: Vec<&str> = frames
            .iter()
            .filter_map(|frame| match frame {
                mogwai_protocol::VenueMessage::OrderRejected {
                    client_order_id, ..
                } => Some(client_order_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(ids, ["A", "B"], "{frames:?}");
        assert_eq!(boundary_frame_count(&group), 2);
    }
}
