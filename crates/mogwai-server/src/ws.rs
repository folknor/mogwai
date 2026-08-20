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
    /// `100.0000001` board the same boat. An unserved speed PLACES a second
    /// boat on the same water rather than being refused - speed mutates no
    /// generated value, so it is a second cursor, not a second river. The one
    /// refusal left is per LEDGER: an account already seated on this river at
    /// another speed would be judged on two clocks.
    #[serde(default)]
    speed: Option<f64>,
    /// Absent means indefinite. SIMULATED milliseconds, measured on the boat's
    /// clock from this passenger's boarding instant and not from boot. A
    /// duration is a property of the passenger, so passengers with different
    /// durations still share one boat; each announces `RunComplete` and closes
    /// at its own deadline, and the boat winds down when the last one leaves.
    #[serde(default)]
    duration_ms: Option<u64>,
    /// The account to trade under. Absent means the venue's default account,
    /// which exists for the ephemeral single-client venue where naming one
    /// would be ceremony - it is NOT a venue-wide account every connection
    /// shares.
    ///
    /// The id is the CLIENT'S and outlives the connection, so presenting the
    /// same one again resumes that ledger. The venue cannot distinguish a
    /// reconnect from a stranger claiming the id and does not try; anyone who
    /// knows an id can claim its account, which is acceptable on a loopback
    /// venue serving one orchestrator's subagents and is stated rather than
    /// assumed.
    #[serde(default)]
    account: Option<String>,
    /// The CLIENT presenting the account, so one client's several sockets on
    /// one ledger are not read as several clients fighting over it.
    ///
    /// A nautilus host dials `/ws` twice - market data and execution - and both
    /// legs carry the same `account` by construction, so without this the second
    /// dial evicts the first and the host disconnects itself. Sockets sharing a
    /// session coexist; a socket presenting a different one, or none, is a new
    /// client and takes the ledger. Absent on both sides is therefore exactly
    /// the pre-session behaviour.
    ///
    /// It is the CLIENT'S string and the venue reads nothing into it beyond
    /// equality: stable across a client's sockets and their redials, fresh in a
    /// restarted process. Like the account id it is a bearer token - anyone who
    /// knows the pair can join that ledger rather than displace it - which is
    /// acceptable on a loopback venue and is stated rather than assumed.
    #[serde(default)]
    session: Option<String>,
}

/// What one connection is bound to, decided before the upgrade completes and
/// owned by the socket for its whole life.
///
/// It exists as a struct rather than a bare `Symbol` because boat placement,
/// the per-boat clock and the ledger attach here, and because every downstream
/// signature then changes exactly once.
#[derive(Debug)]
pub(crate) struct SocketSession {
    pub(crate) symbol: mogwai_protocol::Symbol,
    pub(crate) ticket: Ticket,
    pub(crate) duration_ms: Option<u64>,
    /// The ledger this connection trades on. Resolved before the upgrade, so a
    /// frame cannot reach a handler before the account it books into exists.
    pub(crate) passenger: Arc<crate::run::Passenger>,
    /// The client identity this socket presented, kept on the bound lane so a
    /// later claim on the same account can tell this client's other sockets
    /// from a stranger's. See `Run::evict_account`.
    pub(crate) client_session: Option<String>,
    /// This socket's claim to be reading the account, given up when the socket
    /// is done with it. An `Option` so `handle_socket` can give it up the
    /// instant it has released its lane, rather than at the end of its own
    /// teardown - the boat ticket above must outlive the writer's close frame,
    /// and the account must not be swept for that long.
    pub(crate) admission: Option<crate::run::Admission>,
    /// THIS SESSION'S CLAIM ON THE PROCESS, taken in `ws_upgrade` BEFORE the
    /// 101 is returned and dropped with the session, after the writer has
    /// flushed its close.
    ///
    /// Taken in the upgrade handler rather than at the top of `handle_socket`,
    /// and the difference is a real window rather than tidiness. `handle_socket`
    /// runs in the task `on_upgrade` spawns, which is polled only AFTER hyper's
    /// connection future has already resolved at the 101 - so a guard taken
    /// there is not yet held when `axum::serve` can complete, and a completion
    /// landing in that gap sees `sessions_drained` answer with this session
    /// counted at zero and the runtime drops the task before its first poll.
    /// That is the same defect `sessions_tx` closes, one window smaller. Taken
    /// here it is held before the 101 exists, so there is no instant at which
    /// this connection is upgraded and uncounted.
    ///
    /// Never read. It is a `watch::Receiver` whose LIVENESS is the whole
    /// signal; see `Run::sessions_tx`.
    #[expect(
        dead_code,
        reason = "its LIVENESS is the signal; reading it would be meaningless"
    )]
    pub(crate) alive: tokio::sync::watch::Receiver<()>,
}

/// THE SEAT IS RELEASED HERE, and it has to be here rather than at the freeze.
///
/// A freeze needs every socket on the account gone, so an account riding two
/// rivers that loses one socket never freezes and would hold that seat
/// forever. `BoatKey` carries no placement nonce, so the held seat is
/// indistinguishable from a live one as soon as any account boards that river
/// at that speed again - and the sweeper would then drive this ledger off a
/// boat it never boarded.
///
/// `Drop` rather than a call at the end of `handle_socket`: the session is
/// also dropped when an upgrade is abandoned before the handler ever runs.
///
/// The `Admission` it carries rides along for the same reason and is the half
/// the abandoned upgrade makes necessary: a socket is counted onto its account
/// before the 101, so an upgrade that never reaches `handle_socket` still ends
/// with nothing reading the account - and the departure then freezes it, which
/// is what makes it TTL-collectable and takes it back out of the sweep.
impl Drop for SocketSession {
    fn drop(&mut self) {
        self.passenger.unsit(&self.ticket.boat().key());
    }
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
    // Resolved before the instrument, because the instrument is registered on
    // THIS passenger's ledger. A malformed id is refused here rather than at
    // first order: nautilus cannot construct an `AccountId` from a bare word, so
    // a venue that accepted one would be refused by every consumer later.
    //
    // SEATED, not merely looked up: claiming an account evicts whoever already
    // holds it, because a ledger is never read from two sockets at once and a
    // second socket presenting an id is indistinguishable from that client
    // reconnecting.
    // Bounded and charset-checked before it is stored or compared: it arrives
    // in a URL, is echoed in no frame, and an unbounded one would be per-socket
    // memory a client sets for free.
    if let Some(session) = query.session.as_deref()
        && let Err(reason) = mogwai_protocol::validate_session_id(session)
    {
        return (
            StatusCode::BAD_REQUEST,
            format!("session id is not usable: {reason}"),
        )
            .into_response();
    }
    let session = query.session.as_deref();
    // NOTHING IS CLAIMED YET, and the order below is the whole point: every
    // refusal this handler can make is decided BEFORE `seat`, because seating
    // closes the incumbent's sockets and, under `reset_account_on_reconnect`,
    // discards its ledger. Refusing after that turned any of these five 400s
    // into a one-request, unauthenticated way to disconnect a live client and
    // wipe its position book while never connecting at all -
    // `GET /ws?account=X&speed=NaN` was the cheapest spelling. Eviction is now
    // the LAST thing that happens before the 101.
    let (account_id, claimed) = match &query.account {
        Some(named) => match mogwai_protocol::AccountId::parse(named) {
            Ok(account_id) => (account_id, true),
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    format!("account id is not usable: {error}"),
                )
                    .into_response();
            }
        },
        None => (state.run.default_account_id(), false),
    };
    // The bind-time shape refusal: an invalid resolved shape or a
    // funding-barred one is a CONFIGURATION error, named here and before any
    // trading, rather than surfacing later as a fill-time funds rejection.
    //
    // RESOLVED, not yet registered. Resolution is a property of the venue and
    // is the only fallible half, so it answers here where nothing has been
    // taken from anybody; the ledger-side install happens on the passenger the
    // seat produces, further down.
    let profile = match state.rivers.resolve_profile(&symbol) {
        Ok(profile) => profile,
        Err(error) => return (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    };
    // THIS ACCOUNT'S funding, against the shape it is binding.
    //
    // The boot-time barred set answers the same question for the venue's
    // configured `[balances]`, which is now only what an UNNAMED account opens
    // with - a client that named its own balances is funded in whatever it said,
    // and the venue has no way to know at boot what that will be. So the check
    // moves here for named accounts, where it is still knowable with no order at
    // all and is still a CONFIGURATION error rather than a trading outcome.
    //
    // Presence, never sufficiency: running out is depletion, and a funds
    // rejection on a served shape must keep meaning that and only that.
    //
    // Asked of the ledger this connection WILL get: a seat that resets serves
    // the venue template's balances rather than whatever the account holds now.
    let settlement = profile.def.class.settlement_currency();
    let resetting = state
        .run
        .seat_discards_ledger(&account_id, claimed, session);
    if !state
        .run
        .funded_in(&account_id, resetting, settlement)
        .await
    {
        return (
            StatusCode::BAD_REQUEST,
            format!(
                "account {account} is not funded in {settlement}, which is what {symbol} settles \
                 in; open the account with a {settlement} balance",
                account = account_id.as_str()
            ),
        )
            .into_response();
    }
    let speed = query.speed.unwrap_or(state.cfg.speed);
    if !speed.is_finite() || speed < 0.0 {
        return (
            StatusCode::BAD_REQUEST,
            "speed must be finite and non-negative",
        )
            .into_response();
    }
    let river = state.rivers.resolve_key(&profile);
    let ticket = match state
        .run
        .boatyard
        .board(&BoardRequest { river, speed })
        .await
    {
        Ok(ticket) => ticket,
        Err(BoardRefusal::Placement(err)) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("could not place boat for {symbol}: {err}"),
            )
                .into_response();
        }
    };
    // One ledger, one cadence. Two sockets on the default account can ride
    // two rivers, but two speeds on one river would give that ledger two
    // clocks.
    //
    // UNCONDITIONAL, including for a frozen account. A reseat at a new speed
    // still passes, because a freeze released every seat; routing the frozen
    // case around the check instead would reopen the race the check exists to
    // close, since an account is CREATED frozen and does not attach until its
    // socket reaches `resume` further down - so two first connections would
    // both read themselves frozen and both sit.
    //
    // TAKEN ON THE EXISTING LEDGER, BEFORE THE SEAT, and skipped entirely when
    // the seat is going to reset: a reset ledger holds no seat at all, so
    // asking the outgoing one would refuse exactly the reconnect-at-a-new-speed
    // the reset knob exists to serve. Where the check does apply, the seat is
    // TAKEN here rather than merely tested, so nothing can slip between the
    // test and the claim - and this is the last fallible step, so a seat taken
    // here is never abandoned.
    let mut seated: Option<(Arc<crate::run::Passenger>, crate::run::Admission)> = None;
    if !resetting {
        // The ledger `seat` is about to return, resolved before the eviction so
        // this socket can be COUNTED ON to the account before the incumbent is
        // closed. Without that the incumbent's teardown could win the race to
        // an account with no lane and no admission, freeze it, and make the
        // newcomer's `resume` retire a book it had no business retiring - which
        // would be a nondeterministic behaviour change, not a refusal. The
        // resetting branch needs none of this: the ledger it produces is a
        // fresh one, so a freeze in that window retires nothing.
        let existing = state.run.passenger(&account_id);
        if let Err(sitting) = existing.try_sit(ticket.boat().key()) {
            let sitting_speed = sitting.speed();
            return (
                StatusCode::BAD_REQUEST,
                format!(
                    "account {account} is already seated on {symbol} at speed {sitting_speed}; a \
                     ledger carries one cadence",
                    account = account_id.as_str()
                ),
            )
                .into_response();
        }
        // The guard is TAKEN AND HELD, never raised as a bare count: an
        // abandoned upgrade or a cancelled future between here and the 101 then
        // lowers it on the way out instead of stranding the account
        // permanently counted-in.
        let admission = state.run.admit(&existing);
        seated = Some((existing, admission));
    }
    // EVICTION HAPPENS HERE, with every refusal already decided. `resetting`
    // was evaluated once, above, and is handed to `seat` rather than re-derived
    // there, so this call cannot decide to reset an account the funding and
    // cadence checks were taken against on the assumption that it would not.
    let passenger = state.run.seat(&account_id, claimed, session, resetting);
    let admission = match seated {
        // The ordinary non-resetting path: the ledger checked above is the one
        // the seat produced, so its seat and its admission carry straight into
        // the session.
        Some((existing, admission)) if Arc::ptr_eq(&existing, &passenger) => Some(admission),
        // The ledger MOVED OUT FROM UNDER THE CHECK. `resetting` is false here,
        // so `seat` did not reopen the account itself: only another upgrade
        // racing this same account inside this window can have replaced the map
        // entry. The admission is given up right here - dropping the guard
        // departs the account it was taken on, so nothing is stranded counted-in
        // - and the SEAT taken on `existing` is left behind deliberately, since
        // `existing` is no longer reachable through the passenger map and dies
        // with this Arc. That is stated rather than relied on: the seat is
        // harmless because the ledger holding it is unreachable, not because
        // anything releases it.
        //
        // The resetting path arrives here too, having checked and admitted
        // nothing yet, and takes the same branch below.
        _ => None,
    };
    let admission = match admission {
        Some(admission) => admission,
        None => {
            // A ledger this call minted or reset holds no seat, so this cannot
            // refuse for the cadence rule. It can still lose to another upgrade
            // racing the same account, which is a refusal AFTER the eviction and
            // the one case the ordering above cannot reach - it needs two
            // upgrades interleaved inside this window, where the pre-seat check
            // needs none.
            if let Err(sitting) = passenger.try_sit(ticket.boat().key()) {
                let sitting_speed = sitting.speed();
                return (
                    StatusCode::BAD_REQUEST,
                    format!(
                        "account {account} is already seated on {symbol} at speed \
                         {sitting_speed}; a ledger carries one cadence",
                        account = passenger.account_id.as_str()
                    ),
                )
                    .into_response();
            }
            // Counted onto the account here instead, since the branch above
            // never ran. Either way the socket is counted on BEFORE the upgrade
            // completes and off it when its session drops, which is what keeps
            // the account attached across the gap before `bind_lanes` and what
            // freezes it if this upgrade is abandoned and never binds anything.
            state.run.admit(&passenger)
        }
    };
    // The ledger-side install, on the passenger the seat actually produced.
    state.run.register_instrument(&passenger, &profile).await;
    let session = SocketSession {
        symbol,
        ticket,
        duration_ms: query.duration_ms,
        passenger,
        client_session: query.session.clone(),
        admission: Some(admission),
        // Before the 101, not inside the spawned handler. See the field.
        alive: state.run.session_guard(),
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
    passenger: &Arc<crate::run::Passenger>,
) {
    let class = CommandClass::of(&cmd);
    match process_order_cmd(cmd, state, &state.run, lanes, symbol, boat, passenger).await {
        OrderOutcome::Produced {
            mut events,
            reservation,
        }
        | OrderOutcome::Refused {
            mut events,
            reservation,
        } => {
            // Attribution happens here rather than inside `process_order_cmd`
            // because this is the one place holding both the produced events and
            // the lanes whose id names the submitter.
            //
            // Claim first, then scope: a submit that is immediately queried in
            // the same batch would otherwise have its own row dropped.
            //
            // DO NOT PUT AN `.await` BETWEEN HERE AND `submit_produced`, and the
            // reason is not style. `process_order_cmd` released the engine lock
            // before returning, so the order below is ALREADY VISIBLE to the
            // sweeper while its `OrderAccepted` is still unpublished. Publication
            // order is enqueue order and nothing else: `submit_produced` appends
            // in call order, and the exec pump is one sequential task that sleeps
            // in-line, so it is head-of-line and cannot reorder. A sweep that
            // enqueued a fill for this order first would therefore hand the
            // client a fill for an order it was never told was accepted.
            //
            // That does not happen today, and the protection is TIMING rather
            // than design. The sweeper must gather `pending_scans`, WALK THE
            // TAPE - the dominant cost in the system - re-lock and apply before
            // it can deliver, while this stretch is three cheap synchronous
            // calls that never yield. Inverting them needs this thread preempted
            // for milliseconds inside a window with no yield point in it. Add an
            // await here and that window becomes a scheduling decision instead,
            // at which point the fix is to enqueue while still holding the engine
            // guard rather than to hope.
            let account = passenger.account_id.as_str();
            state.run.track_ownership(&events, account);
            state.run.scope_query_rows(&mut events, account);
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
    passenger: Arc<crate::run::Passenger>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // The permit's scope is the correctness property: the process-wide
        // slot may return only after the command has been ACTED ON, or the
        // bound counts acceptances rather than work in flight. The drop is
        // therefore EXPLICIT, sequenced after the awaited dispatch, rather
        // than left to a destructure binding whose lifetime an underscore
        // pattern would silently end early.
        while let Some(queued) = commands.recv().await {
            dispatch_command(queued.cmd, &state, &lanes, &symbol, &boat, &passenger).await;
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

/// Wall floor under the converted heartbeat period. See the comment at its one
/// use; the sweeper's `MIN_SWEEP_WALL` is the precedent, not the source.
const MIN_HEARTBEAT_WALL: std::time::Duration = std::time::Duration::from_millis(5);

fn current_completion(
    completion: &mut tokio::sync::watch::Receiver<Option<(u64, u64)>>,
) -> Option<(u64, u64)> {
    *completion.borrow_and_update()
}

pub(crate) fn spawn_exec_pump(
    mut exec_rx: mpsc::UnboundedReceiver<HeldFrame>,
    passenger: Arc<crate::run::Passenger>,
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
            let delay = passenger
                .delay_ms
                .load(std::sync::atomic::Ordering::Relaxed)
                .saturating_add(class.map_or(0, |class| passenger.ack_ms(class)));
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
    passenger: Arc<crate::run::Passenger>,
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
                //
                // THIS PASSENGER'S windows, not the venue's: transport havoc
                // corrupts what one connection receives, so arming a blackout on
                // one account must not black out every other account on the
                // exchange.
                if frame.class != FrameClass::Terminal {
                    if passenger.dark.open_at(sim, now) {
                        continue;
                    }
                    if frame.class == FrameClass::MarketData && passenger.stall.open_at(sim, now) {
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

async fn handle_socket(socket: WebSocket, state: AppState, mut session: SocketSession) {
    tracing::info!(symbol = %session.symbol, "socket bound to river");
    // The session's claim on the process - `SocketSession::alive` - is already
    // held, taken before the 101 rather than here. `session` lives to the end
    // of this function, past the writer's close flush, which is where the claim
    // is given up.
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
        Arc::clone(&session.passenger),
    );
    let writer = tokio::spawn(run_writer(
        sink,
        prio_rx,
        out_rx,
        Arc::clone(&session.passenger),
        boat_sim,
    ));
    // Venue-ORIGINATED execution output (a trigger fill nobody commanded)
    // is delivered through these lanes, so the run has to know about them for
    // as long as this connection lives.
    let lane_id = state.run.bind_lanes(
        lanes.clone(),
        session.passenger.account_id.as_str(),
        session.client_session.as_deref(),
    );
    // ATTACHED from here, which is what un-freezes the account and puts it back
    // in the sweep. Bound AFTER the lane, so anything the resume retires has a
    // lane to be delivered on; a returning socket learns what its absence cost
    // rather than discovering a cancelled order by querying.
    let resumed = state
        .run
        .resume(
            &session.passenger,
            &session.symbol,
            crate::config::sim_now_ns(boat_sim),
        )
        .await;
    if !resumed.is_empty() {
        let shape = session.passenger.engine.lock().await.book_shape();
        if let Some(reservation) = lanes.reserve_swept(&shape, resumed.len(), resumed.len()) {
            drop(lanes.submit_produced(reservation, Instant::now(), None, resumed));
        }
    }
    let pump = spawn_exec_pump(
        held_rx,
        Arc::clone(&session.passenger),
        boat_sim,
        out_tx.clone(),
    );
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
        // FLOORED IN WALL TIME, for the same reason `MIN_SWEEP_WALL` exists on
        // the sweep side and with the same 5 ms. The configured period is
        // SIMULATED, so `wall_duration` shrinks it linearly with `speed` while
        // the cost of a beat - a serialization, a channel send, a writer wake -
        // does not, and `wall_duration`'s own floor is one NANOSECOND. At a high
        // speed the heartbeat task therefore degenerated into a
        // timer-granularity loop pushing uncharged frames into a 256-slot
        // channel the peer has to read. Liveness needs a frame now and then, not
        // a frame per timer tick, so the floor costs the signal nothing.
        //
        // ITS OWN CONSTANT, not the sweeper's, though the number matches
        // today: these floor two different costs against two different
        // budgets, and one name shared between them would make a change to
        // either silently move the other.
        let period = boat_sim
            .wall_duration(sim_duration_from_millis(beat_state.cfg.server_heartbeat_ms))
            .max(MIN_HEARTBEAT_WALL);
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
                    code: mogwai_protocol::close::NORMAL,
                    reason: mogwai_protocol::close::RUN_COMPLETE.into(),
                }))
                .await,
        );
    } else {
        // THE VENUE'S OWN CLOSE ENDS THIS LOOP, without waiting on the peer.
        // Every other exit below needs the client to act - its close frame, its
        // EOF, or the run ending - so an evicted socket whose peer ignores the
        // close frame stayed here, and its `SocketSession` held the account's
        // SEAT on that boat. The connection that evicted it was then refused
        // its own reconnect at a different speed. See `ExecLanes::closed`.
        let closed = lanes.closed();
        loop {
            tokio::select! {
                () = closed.notified() => break,
                changed = completion.changed() => {
                    let completed = if changed.is_ok() { *completion.borrow_and_update() } else { None };
                    if completed.is_some() {
                        let (sim_now_ns, elapsed_ns) = completion_on_boat_clock(boat_sim);
                        drop(out_tx.send(Outbound::Frame(OutboundFrame { payload: Arc::from(serde_json::to_string(&ServerMessage::RunComplete { sim_now_ns, elapsed_ns }).expect("RunComplete serializes")), class: FrameClass::Terminal, charge: None, slot: None })).await);
                        drop(out_tx.send(Outbound::Close(CloseSpec { code: mogwai_protocol::close::NORMAL, reason: mogwai_protocol::close::RUN_COMPLETE.into() })).await);
                    }
                    break;
                }
                () = async { if let Some(timer) = duration.as_mut().as_pin_mut() { timer.await } }, if duration.is_some() => {
                    let elapsed_ns = session.duration_ms.unwrap_or(0).saturating_mul(1_000_000);
                    let now = sim_now_ns(boat_sim);
                    drop(out_tx.send(Outbound::Frame(OutboundFrame { payload: Arc::from(serde_json::to_string(&ServerMessage::RunComplete { sim_now_ns: now, elapsed_ns }).expect("RunComplete serializes")), class: FrameClass::Terminal, charge: None, slot: None })).await);
                    drop(out_tx.send(Outbound::Close(CloseSpec { code: mogwai_protocol::close::NORMAL, reason: mogwai_protocol::close::DURATION_COMPLETE.into() })).await);
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
                                    retryable: true,
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
    // Given up HERE rather than with the session, which outlives this for as
    // long as the writer needs to flush its close frame: the account is no
    // longer being read the moment its lane is gone, and leaving it counted-in
    // for the writer's grace would keep it in the sweep for that long. The
    // session still carries the guard, so an abandoned upgrade that never got
    // here is covered by the drop.
    drop(session.admission.take());
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
