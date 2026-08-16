// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! A websocket passenger on one boat, plus its run-wide command ledger.

use std::{sync::Arc, time::Instant};

use axum::{
    extract::{
        Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
};
use futures_util::{SinkExt, StreamExt};
use mogwai_protocol::{ClientMessage, CommandClass, ServerMessage, truncate_reason};
use tokio::sync::{OwnedSemaphorePermit, mpsc};

use crate::{
    admission::{CloseSpec, ExecLanes, FrameClass, HeldFrame, Outbound, OutboundFrame},
    boatyard::{BoardRefusal, BoardRequest, Ticket},
    config::{build_admission_limits, sim_duration_from_millis, sim_now_ns},
    http::{AppState, OrderOutcome, process_order_cmd, resolve_socket_symbol},
};

/// The upgrade's query string, exactly as the client wrote it.
///
/// `deny_unknown_fields` is a WIRE-COMPATIBILITY decision, taken knowingly: a
/// client that sends a key this carrier does not handle is REFUSED rather than
/// silently served a different river, speed or duration than it asked for. The
/// price is that ANY unrecognized key is a `400`, including
/// one an unrelated client, proxy or tracing layer appends, and including a
/// future key added before its handling lands. That is accepted:
/// accepted-and-ignored is the failure mode this carrier exists to prevent, and
/// the venue's clients are its own. Relaxing it later is a wire change that owes
/// its own reasoning, not a tidy-up.
///
/// A repeated `symbol` key is NOT an error - `serde_urlencoded` keeps the last
/// occurrence - so the last one wins and is then validated like any other.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SocketQuery {
    /// Absent means "the run's boot symbol", which is what every client that
    /// predates this carrier sends.
    #[serde(default)]
    symbol: Option<String>,
    /// Absent means the venue's configured `speed`. Finite and non-negative,
    /// quantized to micro-multiples in the sharing key, so `100` and
    /// `100.0000001` board the same boat. A speed differing from the one the
    /// river's sitting boat carries is REFUSED before the 101, naming that
    /// speed; it never places a second boat on the same water.
    #[serde(default)]
    speed: Option<f64>,
    /// Absent means indefinite. SIMULATED milliseconds, measured on the boat's
    /// clock from this passenger's boarding instant and not from boot. A
    /// duration is a property of the passenger, so passengers with different
    /// durations still share one boat; each announces `RunComplete` and closes
    /// at its own deadline, and the boat winds down when the last one leaves.
    #[serde(default)]
    duration_ms: Option<u64>,
}

/// What one connection is bound to, decided before the upgrade completes and
/// owned by the socket for its whole life.
///
/// One field today. It exists as a struct rather than a bare `Symbol` because
/// boat placement and the per-boat clock attach here, and because every
/// downstream signature then changes exactly once.
#[derive(Debug)]
pub(crate) struct SocketSession {
    pub(crate) symbol: mogwai_protocol::Symbol,
    pub(crate) ticket: Ticket,
    pub(crate) duration_ms: Option<u64>,
}

/// Bind one socket to one river, or refuse before the 101.
///
/// A refusal is a STATUS, not a close code: an unserved symbol on `/trades` is
/// a `400` naming what is served, and a WebSocket close after a successful
/// upgrade is the "looks like an outage" ambiguity `CLOSE_VENUE_FAULT` fights.
/// Returning here spawns no task, allocates no lane and opens no socket.
///
/// The extractor order is CONVENTION matching the other handlers, not a
/// constraint: all three are `FromRequestParts`, so any order compiles.
pub(crate) async fn ws_upgrade(
    ws: WebSocketUpgrade,
    Query(query): Query<SocketQuery>,
    State(state): State<AppState>,
) -> Response {
    let symbol = match resolve_socket_symbol(query.symbol.as_deref(), &state.run.boot_symbol) {
        Ok(symbol) => symbol,
        Err(body) => return (StatusCode::BAD_REQUEST, body).into_response(),
    };
    // The bind-time shape refusal: an invalid resolved shape or a
    // funding-barred one is a CONFIGURATION error, named here and before any
    // trading, rather than surfacing later as a fill-time funds rejection.
    let profile = match state.run.ensure_instrument(&symbol).await {
        Ok(profile) => profile,
        Err(error) => return (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    };
    let speed = query.speed.unwrap_or(state.cfg.speed);
    if !speed.is_finite() || speed < 0.0 {
        return (
            StatusCode::BAD_REQUEST,
            "speed must be finite and non-negative",
        )
            .into_response();
    }
    let river = state.rivers.resolve_key(&profile);
    let ticket = match state.run.boatyard.board(&BoardRequest { river, speed }).await {
        Ok(ticket) => ticket,
        Err(BoardRefusal::SpeedInUse { sitting_speed }) => return (StatusCode::BAD_REQUEST, format!("river {symbol} already has a boat at speed {sitting_speed}; request that speed or wait for its passengers to leave")).into_response(),
        Err(BoardRefusal::Placement(err)) => return (StatusCode::BAD_REQUEST, format!("could not place boat for {symbol}: {err}")).into_response(),
    };
    let session = SocketSession {
        symbol,
        ticket,
        duration_ms: query.duration_ms,
    };
    ws.max_message_size(mogwai_protocol::MAX_CLIENT_MESSAGE_BYTES)
        .max_frame_size(mogwai_protocol::MAX_CLIENT_MESSAGE_BYTES)
        .on_upgrade(move |socket| handle_socket(socket, state, session))
}

fn send_admission(lanes: &ExecLanes, msg: ServerMessage) -> Result<(), CloseSpec> {
    let Some(ticket) = lanes.reserve_admission() else {
        return Err(CloseSpec::overload(
            "priority admission lane saturated: the peer is not reading",
        ));
    };
    lanes
        .emit_admission(ticket, msg)
        .map_err(|_| CloseSpec::overload("outbound writer is gone"))
}

struct QueuedCommand {
    cmd: ClientMessage,
    /// The process-wide command slot, deliberately NOT underscore-prefixed:
    /// the dispatcher drops it EXPLICITLY after the command has been acted on,
    /// so an edit that releases it early has to delete a visible `drop`, not
    /// merely rename a binding in a destructure pattern.
    global_slot: OwnedSemaphorePermit,
}

async fn dispatch_command(
    cmd: ClientMessage,
    state: &AppState,
    lanes: &ExecLanes,
    symbol: &mogwai_protocol::Symbol,
    boat: &Arc<crate::boatyard::Boat>,
) {
    let class = CommandClass::of(&cmd);
    match process_order_cmd(cmd, state, &state.run, lanes, symbol, boat).await {
        OrderOutcome::Produced {
            events,
            reservation,
        }
        | OrderOutcome::Refused {
            events,
            reservation,
        } => {
            // Before the batch leaves: whatever it accepted is now owned by THIS
            // connection, and whatever it ended is owned by nobody. Recorded
            // here rather than inside `process_order_cmd` because this is the
            // one place that holds both the produced events and the lanes whose
            // id names the submitter.
            state.run.track_ownership(&events, lanes.id());
            drop(lanes.submit_produced(reservation, Instant::now(), class, events));
        }
        OrderOutcome::NotAdmitted(frame) | OrderOutcome::Diagnostic(frame) => {
            drop(send_admission(lanes, frame));
        }
    }
}

fn spawn_command_dispatcher(
    mut commands: mpsc::Receiver<QueuedCommand>,
    state: AppState,
    lanes: ExecLanes,
    symbol: mogwai_protocol::Symbol,
    boat: Arc<crate::boatyard::Boat>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // The permit's scope is the correctness property: the process-wide
        // slot may return only after the command has been ACTED ON, or the
        // bound counts acceptances rather than work in flight. The drop is
        // therefore EXPLICIT, sequenced after the awaited dispatch, rather
        // than left to a destructure binding whose lifetime an underscore
        // pattern would silently end early.
        while let Some(queued) = commands.recv().await {
            dispatch_command(queued.cmd, &state, &lanes, &symbol, &boat).await;
            drop(queued.global_slot);
        }
    })
}

/// Re-stamp the venue's completion on THIS socket's clock.
///
/// The venue deadline is a venue property and is measured on the venue clock -
/// there is no boat to ask when the last passenger has left. But the frame goes
/// to a socket whose every other stamp is its boat's, so the venue instant is
/// the SIGNAL and this is the conversion. `elapsed_ns` is how much tape THIS
/// passenger's boat covered, which is the only elapsed number meaningful to the
/// reader; a boat placed after the deadline covered none, hence the clamp.
fn completion_on_boat_clock(boat_sim: mogwai_protocol::SimClock) -> (u64, u64) {
    let now = sim_now_ns(boat_sim);
    (now, now.saturating_sub(boat_sim.sim_epoch_ns))
}

fn current_completion(
    completion: &mut tokio::sync::watch::Receiver<Option<(u64, u64)>>,
) -> Option<(u64, u64)> {
    *completion.borrow_and_update()
}

pub(crate) fn spawn_exec_pump(
    mut exec_rx: mpsc::UnboundedReceiver<HeldFrame>,
    run: Arc<crate::run::Run>,
    sim: mogwai_protocol::SimClock,
    out: mpsc::Sender<Outbound>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(HeldFrame {
            arrived,
            class,
            frame,
        }) = exec_rx.recv().await
        {
            let delay = run
                .delay_ms
                .load(std::sync::atomic::Ordering::Relaxed)
                .saturating_add(class.map_or(0, |class| run.ack_ms(class)));
            if delay > 0 {
                let hold = sim.wall_duration(sim_duration_from_millis(delay));
                let remaining = hold.saturating_sub(arrived.elapsed());
                if !remaining.is_zero() {
                    tokio::time::sleep(remaining).await;
                }
            }
            if out.send(Outbound::Frame(frame)).await.is_err() {
                break;
            }
        }
    })
}

async fn run_writer(
    mut sink: futures_util::stream::SplitSink<WebSocket, Message>,
    mut prio_rx: mpsc::UnboundedReceiver<Outbound>,
    mut out_rx: mpsc::Receiver<Outbound>,
    state: AppState,
    sim: mogwai_protocol::SimClock,
) {
    let mut priority_open = true;
    let mut held_open = true;
    while priority_open || held_open {
        let outbound = tokio::select! {
            biased;
            message = prio_rx.recv(), if priority_open => match message { Some(message) => message, None => { priority_open = false; continue; } },
            message = out_rx.recv(), if held_open => match message { Some(message) => message, None => { held_open = false; continue; } },
        };
        match outbound {
            Outbound::Close(close) => {
                drop(
                    sink.send(Message::Close(Some(axum::extract::ws::CloseFrame {
                        code: close.code,
                        reason: close.reason.into(),
                    })))
                    .await,
                );
                break;
            }
            Outbound::Frame(frame) => {
                let now = sim_now_ns(sim);
                // A terminal announcement outranks every armed window: see
                // `FrameClass`. Everything else is gated - `GoDark` wholesale,
                // `StallData` on market data alone.
                if frame.class != FrameClass::Terminal {
                    if state.run.dark.open_at(sim, now) {
                        continue;
                    }
                    if frame.class == FrameClass::MarketData && state.run.stall.open_at(sim, now) {
                        continue;
                    }
                }
                if sink
                    .send(Message::Text(frame.payload.as_ref().into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        }
    }
}

async fn handle_socket(socket: WebSocket, state: AppState, session: SocketSession) {
    tracing::info!(symbol = %session.symbol, "socket bound to river");
    let (sink, mut stream) = socket.split();
    let (out_tx, out_rx) = mpsc::channel(256);
    let (held_tx, held_rx) = mpsc::unbounded_channel();
    let (prio_tx, prio_rx) = mpsc::unbounded_channel();
    let lanes = ExecLanes::new(held_tx, prio_tx, build_admission_limits(&state.cfg));
    let (command_tx, command_rx) = mpsc::channel(state.cfg.pending_command_acts);
    let boat_sim = session.ticket.boat().sim;
    let dispatcher = spawn_command_dispatcher(
        command_rx,
        state.clone(),
        lanes.clone(),
        Arc::clone(&session.symbol),
        Arc::clone(session.ticket.boat()),
    );
    let writer = tokio::spawn(run_writer(sink, prio_rx, out_rx, state.clone(), boat_sim));
    // Venue-ORIGINATED execution output (a trigger fill nobody commanded)
    // is delivered through these lanes, so the run has to know about them for
    // as long as this connection lives.
    let lane_id = state.run.bind_lanes(lanes.clone());
    let pump = spawn_exec_pump(held_rx, Arc::clone(&state.run), boat_sim, out_tx.clone());
    let feed = {
        // This connection's own boat, and its own ring: a busy river cannot
        // lag a passenger subscribed to a quiet one, and a lag here is a
        // property of the water this socket asked for.
        let (mut tape, snapshot) = session.ticket.boat().tape.subscribe_with_snapshot();
        let out_tx = out_tx.clone();
        // The ONE diagnostic this feed can ever emit is `FeedLagged`, and it is
        // emitted precisely when the connection is already drowning in market
        // data. Reserving the priority-lane capacity for it up front, and
        // spending it on the priority lane rather than queueing it behind the
        // backlog that caused it, is the whole reason the promise pool exists.
        let mut fault_promise = lanes.reserve_promise();
        let fault_lanes = lanes.clone();
        tokio::spawn(async move {
            let mut last_market_ts = snapshot.as_ref().map(|frame| frame.ts_event);
            if let Some(frame) = snapshot
                && out_tx
                    .send(Outbound::Frame(OutboundFrame {
                        payload: frame.payload,
                        class: FrameClass::MarketData,
                        charge: None,
                        slot: None,
                    }))
                    .await
                    .is_err()
            {
                return;
            }
            loop {
                match tape.recv().await {
                    Ok(frame) => {
                        if last_market_ts.is_some_and(|prior| frame.ts_event < prior) {
                            tracing::error!(
                                prior_ts_event = last_market_ts,
                                frame_ts_event = frame.ts_event,
                                "VENUE FAULT: tape feed moved backward in event time; killing \
                                 the connection rather than silently ending market data"
                            );
                            drop(fault_lanes.send_close(CloseSpec::venue_fault(format!(
                                "venue fault: tape event time moved backward from {:?} to {}",
                                last_market_ts, frame.ts_event
                            ))));
                            break;
                        }
                        last_market_ts = Some(frame.ts_event);
                        if out_tx
                            .send(Outbound::Frame(OutboundFrame {
                                payload: frame.payload,
                                class: FrameClass::MarketData,
                                charge: None,
                                slot: None,
                            }))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    // A VENUE FAULT, not a divergence and not a refusal the
                    // client could have planned for: the ring turned over, so
                    // the venue has lost market data it already promised to
                    // deliver in ascending order. The connection dies rather
                    // than serving past the hole, because a forward-validation
                    // run that keeps streaming with silently-missing ticks
                    // yields a result that looks clean and is not.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::error!(
                            skipped,
                            "VENUE FAULT: tape ring turned over and the venue lost market \
                             data it had promised; killing the connection rather than \
                             serving a hole"
                        );
                        // Both frames ride the PRIORITY lane: the held lane is
                        // full of exactly the backlog that caused this, so a
                        // diagnostic queued behind it arrives after the socket
                        // is gone, if at all.
                        if let Some(promise) = fault_promise.take() {
                            drop(promise);
                            if let Some(slot) = fault_lanes.reserve_admission() {
                                drop(fault_lanes.emit_admission(
                                    slot,
                                    ServerMessage::FeedLagged {
                                        skipped,
                                        sim_now_ns: sim_now_ns(boat_sim),
                                    },
                                ));
                            } else {
                                tracing::warn!(
                                    "priority lane full; feed-lag diagnostic undeliverable"
                                );
                            }
                        }
                        drop(fault_lanes.send_close(CloseSpec::venue_fault(format!(
                            "venue fault: lost {skipped} ticks; the tape ring turned over \
                             and this feed has an unarmed gap"
                        ))));
                        break;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        })
    };
    // Server-originated liveness. Survives `StallData` (it is not market data)
    // but not `GoDark` (which gates the writer wholesale), which is exactly the
    // distinction it exists to make observable: a stalled feed and a dead venue
    // must not look the same to a client.
    let heartbeat = (state.cfg.server_heartbeat_ms > 0).then(|| {
        let out_tx = out_tx.clone();
        let beat_state = state.clone();
        let period =
            boat_sim.wall_duration(sim_duration_from_millis(beat_state.cfg.server_heartbeat_ms));
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(period).await;
                let frame = OutboundFrame {
                    payload: Arc::from(
                        serde_json::to_string(&ServerMessage::Heartbeat {
                            ts_event: sim_now_ns(boat_sim),
                        })
                        .expect("Heartbeat serializes"),
                    ),
                    class: FrameClass::Execution,
                    charge: None,
                    slot: None,
                };
                if out_tx.send(Outbound::Frame(frame)).await.is_err() {
                    break;
                }
            }
        })
    });
    let mut completion = state.run.completion();
    let already_complete = current_completion(&mut completion);
    let duration = session
        .duration_ms
        .map(|ms| tokio::time::sleep(boat_sim.wall_duration(sim_duration_from_millis(ms))));
    tokio::pin!(duration);
    // The venue's own `(sim_now_ns, elapsed_ns)` pair is deliberately DISCARDED
    // here and below: only the fact of completion crosses, the numbers are
    // re-derived on this socket's boat clock.
    if already_complete.is_some() {
        let (sim_now_ns, elapsed_ns) = completion_on_boat_clock(boat_sim);
        drop(
            out_tx
                .send(Outbound::Frame(OutboundFrame {
                    payload: Arc::from(
                        serde_json::to_string(&ServerMessage::RunComplete {
                            sim_now_ns,
                            elapsed_ns,
                        })
                        .expect("RunComplete serializes"),
                    ),
                    class: FrameClass::Terminal,
                    charge: None,
                    slot: None,
                }))
                .await,
        );
        drop(
            out_tx
                .send(Outbound::Close(CloseSpec {
                    code: 1000,
                    reason: "run complete".into(),
                }))
                .await,
        );
    } else {
        loop {
            tokio::select! {
                changed = completion.changed() => {
                    let completed = if changed.is_ok() { *completion.borrow_and_update() } else { None };
                    if completed.is_some() {
                        let (sim_now_ns, elapsed_ns) = completion_on_boat_clock(boat_sim);
                        drop(out_tx.send(Outbound::Frame(OutboundFrame { payload: Arc::from(serde_json::to_string(&ServerMessage::RunComplete { sim_now_ns, elapsed_ns }).expect("RunComplete serializes")), class: FrameClass::Terminal, charge: None, slot: None })).await);
                        drop(out_tx.send(Outbound::Close(CloseSpec { code: 1000, reason: "run complete".into() })).await);
                    }
                    break;
                }
                () = async { if let Some(timer) = duration.as_mut().as_pin_mut() { timer.await } }, if duration.is_some() => {
                    let elapsed_ns = session.duration_ms.unwrap_or(0).saturating_mul(1_000_000);
                    let now = sim_now_ns(boat_sim);
                    drop(out_tx.send(Outbound::Frame(OutboundFrame { payload: Arc::from(serde_json::to_string(&ServerMessage::RunComplete { sim_now_ns: now, elapsed_ns }).expect("RunComplete serializes")), class: FrameClass::Terminal, charge: None, slot: None })).await);
                    drop(out_tx.send(Outbound::Close(CloseSpec { code: 1000, reason: "passenger duration complete".into() })).await);
                    break;
                }
                message = stream.next() => match message {
                    Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                    Some(Ok(Message::Text(text))) => match serde_json::from_str::<ClientMessage>(&text) {
                        Ok(command) => {
                            let subject = crate::http::admission_subject(&command);
                            let queued = Arc::clone(&state.pending_commands)
                                .try_acquire_owned()
                                .ok()
                                .map(|global_slot| QueuedCommand { cmd: command, global_slot });
                            if queued.is_none_or(|queued| command_tx.try_send(queued).is_err()) {
                                drop(send_admission(&lanes, ServerMessage::AdmissionRejected {
                                    subject,
                                    reason: "venue command capacity exhausted".into(),
                                    ts_event: sim_now_ns(boat_sim),
                                }));
                            }
                        }
                        Err(err) => { drop(send_admission(&lanes, ServerMessage::ProtocolError { reason: truncate_reason(format!("invalid client frame: {err}")), ts_event: sim_now_ns(boat_sim) })); }
                    },
                    Some(Ok(Message::Binary(_))) => {
                        drop(send_admission(&lanes, ServerMessage::ProtocolError {
                            reason: "binary client frames are unsupported; send JSON text".into(),
                            ts_event: sim_now_ns(boat_sim),
                        }));
                    }
                    Some(Ok(_)) => {}
                }
            }
        }
    }
    state.run.release_lanes(lane_id);
    feed.abort();
    pump.abort();
    dispatcher.abort();
    if let Some(heartbeat) = heartbeat {
        heartbeat.abort();
    }
    drop(out_tx);
    drop(lanes);
    let mut writer = writer;
    if tokio::time::timeout(crate::admission::CLOSE_GRACE, &mut writer)
        .await
        .is_err()
    {
        writer.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::current_completion;

    /// The differential is the point: a receiver subscribed AFTER the terminal
    /// transition never sees a change, so a socket that only awaits
    /// `changed()` waits forever on a run that is already over. Both halves are
    /// asserted here, because asserting only the second would pass against the
    /// buggy shape too.
    #[tokio::test]
    async fn receiver_created_after_completion_observes_terminal_state() {
        let (tx, _keep) = tokio::sync::watch::channel(None);
        tx.send_replace(Some((123, 45)));
        let mut late = tx.subscribe();

        let mut awaiting = tx.subscribe();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(200), awaiting.changed(),)
                .await
                .is_err(),
            "a late receiver cannot reach the terminal state by waiting for a change"
        );

        assert_eq!(current_completion(&mut late), Some((123, 45)));
    }
}
