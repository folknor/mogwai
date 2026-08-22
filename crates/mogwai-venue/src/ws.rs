// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! A websocket passenger on one boat, plus its account's run-wide command
//! ledger.

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
use mogwai_protocol::{Command, CommandClass, VenueMessage, truncate_reason};
use tokio::sync::{OwnedSemaphorePermit, mpsc};

use crate::{
    admission::{CloseSpec, ExecLanes, FrameClass, HeldFrame, Outbound, OutboundFrame},
    boatyard::{BoardRefusal, BoardRequest, Ticket},
    config::{build_admission_limits, sim_duration_from_millis, sim_now_ns},
    http::{AppState, OrderOutcome, process_order_cmd, resolve_socket_symbol},
    tape::TapeFrame,
};

/// Market-view loss this passenger has not been told about yet.
///
/// It exists because the venue cannot always speak at the moment it discovers a
/// hole: the declaration is positional, so it waits for the boundary it
/// describes - the next market frame that actually reaches the consumer.
/// Further loss discovered while one is outstanding folds into it, which is what
/// keeps a passenger that is continuously behind from being told once per
/// overwritten frame.
struct PendingGap {
    /// Frames the ring overwrote, across every lag folded into this episode.
    skipped: u64,
    /// The last market frame that CROSSED THE SOCKET before the loss. `None`
    /// when the loss preceded this passenger's first delivered frame, which is
    /// why it is not merely the last frame the venue read.
    after_ts_event: Option<u64>,
}

/// The upgrade's query string, exactly as the consumer wrote it.
///
/// `deny_unknown_fields` is a WIRE-COMPATIBILITY decision, taken knowingly: a
/// consumer that sends a key this carrier does not handle is REFUSED rather than
/// silently served a different river, speed or duration than it asked for. The
/// price is that ANY unrecognized key is a `400`, including
/// one an unrelated consumer, proxy or tracing layer appends, and including a
/// future key added before its handling lands. That is accepted:
/// accepted-and-ignored is the failure mode this carrier exists to prevent, and
/// the venue's consumers are its own. Relaxing it later is a wire change that owes
/// its own reasoning, not a tidy-up.
///
/// A repeated `symbol` key is NOT an error - `serde_urlencoded` keeps the last
/// occurrence - so the last one wins and is then validated like any other.
///
/// The identity key was `session` until the callsign ruling retired `session`
/// as a name for anything but the trading day. `deny_unknown_fields` is what
/// makes that break LOUD for a consumer still sending the old spelling: it is a
/// `400` naming the key rather than a socket silently admitted with no identity
/// and the always-evict reading. Pinned by
/// `the_retired_session_query_key_is_refused`.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SocketQuery {
    /// Absent means "the run's boot symbol", which is what every consumer that
    /// predates this carrier sends.
    #[serde(default)]
    symbol: Option<String>,
    /// Absent means the venue's configured `speed`. Finite and non-negative,
    /// quantized to micro-multiples in the sharing key, so `100` and
    /// `100.0000001` board the same boat. An unserved speed PLACES a second
    /// boat on the same water rather than being refused - speed mutates no
    /// generated value, so it is a second cursor, not a second river. The one
    /// refusal left is per ledger: an account already riding this river at
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
    /// which exists for the ephemeral single-consumer venue where naming one
    /// would be ceremony - it is NOT a venue-wide account every connection
    /// shares.
    ///
    /// The id is the consumer's and outlives the connection, so presenting the
    /// same one again resumes that ledger. The venue cannot distinguish a
    /// reconnect from a stranger claiming the id and does not try; anyone who
    /// knows an id can claim its account, which is acceptable on a loopback
    /// venue serving one orchestrator's subagents and is stated rather than
    /// assumed.
    #[serde(default)]
    account: Option<String>,
    /// The identity this socket presents, so several sockets presenting the
    /// same value can coexist on one ledger.
    ///
    /// A nautilus host dials `/ws` twice - market data and execution - and both
    /// legs carry the same `account` by construction, so without this the second
    /// dial evicts the first and the host disconnects itself. Sockets sharing a
    /// callsign coexist; a socket presenting a different one, or none, takes the
    /// ledger. Absent on both sides is therefore exactly
    /// the pre-callsign behaviour.
    ///
    /// The venue reads nothing into the string beyond equality: it is stable
    /// across related sockets and their redials, and fresh in a
    /// restarted process. Like the account id it is a bearer token - anyone who
    /// knows the pair can join that ledger rather than displace it - which is
    /// acceptable on a loopback venue and is stated rather than assumed.
    #[serde(default)]
    callsign: Option<String>,
}

/// One connected trader: a single websocket under an account, boarded onto one
/// boat, decided before the upgrade completes and owned by the socket for its
/// whole life. It dies with that socket; the ledger, the risk state and the
/// freeze stamp stay on the `Account` it trades under.
///
/// It exists as a struct rather than a bare `Symbol` because boat placement,
/// the per-boat clock and the ledger attach here, and because every downstream
/// signature then changes exactly once.
#[derive(Debug)]
pub(crate) struct Passenger {
    pub(crate) symbol: mogwai_protocol::Symbol,
    pub(crate) ticket: Ticket,
    pub(crate) duration_ms: Option<u64>,
    /// The account this passenger trades under. Resolved before the upgrade, so a
    /// frame cannot reach a handler before the account it books into exists.
    pub(crate) account_state: Arc<crate::run::Account>,
    /// The identity this socket presented, kept on the bound lane so a later
    /// claim on the same account can tell related sockets
    /// from a stranger's. See `Run::evict_account`.
    pub(crate) presented_identity: Option<String>,
    /// This socket's claim to be reading the account, given up when the socket
    /// is done with it. An `Option` so `handle_socket` can give it up the
    /// instant it has released its lane, rather than at the end of its own
    /// teardown - the boat ticket above must outlive the writer's close frame,
    /// and the account must not be swept for that long.
    pub(crate) attach: Option<crate::run::Attach>,
    /// This passenger's claim on the process, taken in `ws_upgrade` before the
    /// 101 is returned and dropped with the passenger, after the writer has
    /// flushed its close.
    ///
    /// Taken in the upgrade handler rather than at the top of `handle_socket`,
    /// and the difference is a real window rather than tidiness. `handle_socket`
    /// runs in the task `on_upgrade` spawns, which is polled only AFTER hyper's
    /// connection future has already resolved at the 101 - so a guard taken
    /// there is not yet held when `axum::serve` can complete, and a completion
    /// landing in that gap sees `passengers_drained` answer with this passenger
    /// counted at zero and the runtime drops the task before its first poll.
    /// That is the same defect `passengers_tx` closes, one window smaller. Taken
    /// here it is held before the 101 exists, so there is no instant at which
    /// this connection is upgraded and uncounted.
    ///
    /// Never read. It is a `watch::Receiver` whose LIVENESS is the whole
    /// signal; see `Run::passengers_tx`.
    #[expect(
        dead_code,
        reason = "its LIVENESS is the signal; reading it would be meaningless"
    )]
    pub(crate) alive: tokio::sync::watch::Receiver<()>,
}

/// THE RIDE ENDS HERE, and it has to be here rather than at the freeze.
///
/// A freeze needs every socket on the account gone, so an account riding two
/// rivers that loses one passenger never freezes and would hold that ride
/// forever. `BoatKey` carries no placement nonce, so a stale ride is
/// indistinguishable from a live one as soon as any account boards that river
/// at that speed again - and the sweeper would then drive this ledger off a
/// boat it never boarded.
///
/// `Drop` rather than a call at the end of `handle_socket`: the passenger is
/// also dropped when an upgrade is abandoned before the handler ever runs.
///
/// The `Attach` it carries rides along for the same reason and is the half
/// the abandoned upgrade makes necessary: a socket is counted onto its account
/// before the 101, so an upgrade that never reaches `handle_socket` still ends
/// with nothing reading the account - and the departure then freezes it, which
/// is what makes it TTL-collectable and takes it back out of the sweep.
impl Drop for Passenger {
    fn drop(&mut self) {
        self.account_state.unsit(&self.ticket.boat().key());
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
    let symbol = match resolve_socket_symbol(query.symbol.as_deref(), &state.run.default_symbol) {
        Ok(symbol) => symbol,
        Err(body) => return (StatusCode::BAD_REQUEST, body).into_response(),
    };
    // Resolved before the instrument, because the instrument is registered on
    // THIS account's ledger. A malformed id is refused here rather than at
    // first order: nautilus cannot construct an `AccountId` from a bare word, so
    // a venue that accepted one would be refused by every consumer later.
    //
    // CLAIMED, not merely looked up: claiming an account evicts whoever already
    // holds it. Sockets presenting the same account and callsign coexist, while
    // a different or absent callsign is a new claim on the account.
    // Bounded and charset-checked before it is stored or compared: it arrives
    // in a URL, is echoed in no frame, and an unbounded one would be per-socket
    // memory a consumer sets for free.
    if let Some(callsign) = query.callsign.as_deref()
        && let Err(reason) = mogwai_protocol::validate_callsign(callsign)
    {
        return (
            StatusCode::BAD_REQUEST,
            format!("callsign is not usable: {reason}"),
        )
            .into_response();
    }
    let callsign = query.callsign.as_deref();
    // NOTHING IS CLAIMED YET, and the order below is the whole point: every
    // refusal this handler can make is decided BEFORE `claim_account`, because claiming
    // closes the incumbent's sockets and, under `reset_account_on_reconnect`,
    // discards its ledger. Refusing after that turned any of these five 400s
    // into a one-request, unauthenticated way to disconnect a live consumer and
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
    // taken from anybody; the ledger-side install happens on the account the
    // claim returns, further down.
    let profile = match state.rivers.resolve_profile(&symbol) {
        Ok(profile) => profile,
        Err(error) => return (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    };
    // THIS ACCOUNT'S funding, against the shape it is binding.
    //
    // The boot-time barred set answers the same question for the venue's
    // configured `[balances]`, which is now only what an UNNAMED account opens
    // with - a consumer that named its own balances is funded in whatever it said,
    // and the venue has no way to know at boot what that will be. So the check
    // moves here for named accounts, where it is still knowable with no order at
    // all and is still a CONFIGURATION error rather than a trading outcome.
    //
    // Presence, never sufficiency: running out is depletion, and a funds
    // rejection on a served shape must keep meaning that and only that.
    //
    // Asked of the ledger this connection will get: a claim that resets serves
    // the configured opening balances rather than whatever the account holds
    // now.
    let settlement = profile.def.class.settlement_currency();
    let resetting = state
        .run
        .claim_discards_ledger(&account_id, claimed, callsign);
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
        // MISCLASSIFIED, AND KNOWINGLY SO UNTIL THE FAULT VOCABULARY EXISTS.
        // Placement performs the river's first materialization, so a failure here
        // is usually a generator that could not produce a shape config already
        // validated - a venue fault, not a bad request. It reaches the consumer
        // as a 400 and never reaches the tape fault channel or `/health`, because
        // placement runs BEFORE `Tape::start` installs the fault sender, and
        // because `TickFault` has no variant for a materialization that could not
        // proceed. Every river has always failed this way; the riverless boot
        // merely makes it the only path for the default label too. Closing it
        // means giving the run a latched materialization fault that follows the
        // terminal path and answers `/health`, and handing every waiter on the
        // same placement that terminal rather than this refusal.
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
    // UNCONDITIONAL, including for a frozen account. A return at a new speed
    // still passes, because a frozen account has no passengers left; routing
    // the frozen
    // case around the check instead would reopen the race the check exists to
    // close, since an account is CREATED frozen and does not attach until its
    // socket reaches `resume` further down - so two first connections would
    // both read themselves frozen and both board.
    //
    // TAKEN ON THE EXISTING LEDGER, BEFORE THE CLAIM, and skipped entirely when
    // the claim is going to reset: a reset ledger has no passengers at all, so
    // asking the outgoing one would refuse exactly the reconnect-at-a-new-speed
    // the reset knob exists to serve. Where the check does apply, the ride is
    // recorded here rather than merely tested, so nothing can slip between the
    // test and the claim, and this is the last fallible step, so a ride recorded
    // here is never abandoned.
    let mut prepared_existing: Option<(Arc<crate::run::Account>, crate::run::Attach)> = None;
    if !resetting {
        // The account `claim_account` is about to return, resolved before the eviction so
        // this socket can be COUNTED ON to the account before the incumbent is
        // closed. Without that the incumbent's teardown could win the race to
        // an account with no lane and no attach, freeze it, and make the
        // newcomer's `resume` retire a book it had no business retiring - which
        // would be a nondeterministic behaviour change, not a refusal. The
        // resetting branch needs none of this: the ledger it produces is a
        // fresh one, so a freeze in that window retires nothing.
        let existing = state.run.account(&account_id);
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
        let attach = state.run.attach(&existing);
        prepared_existing = Some((existing, attach));
    }
    // EVICTION HAPPENS HERE, with every refusal already decided. `resetting`
    // was evaluated once, above, and is handed to `claim_account` rather than
    // re-derived there, so this call cannot decide to reset an account the
    // funding and cadence checks were taken against on the assumption that it
    // would not.
    let account_state = state
        .run
        .claim_account(&account_id, claimed, callsign, resetting);
    let attach = match prepared_existing {
        // The ordinary non-resetting path: the ledger checked above is the one
        // the claim returned, so its ride and its attach carry straight into
        // the passenger.
        Some((existing, attach)) if Arc::ptr_eq(&existing, &account_state) => Some(attach),
        // The ledger MOVED OUT FROM UNDER THE CHECK. `resetting` is false here,
        // so `claim_account` did not reopen the account itself: only another upgrade
        // racing this same account inside this window can have replaced the map
        // entry. The attach is given up right here - dropping the guard
        // departs the account it was taken on, so nothing is stranded counted-in
        // - and the ride recorded on `existing` is left behind deliberately, since
        // `existing` is no longer reachable through the account map and dies
        // with this Arc. That is stated rather than relied on: the ride is
        // harmless because the ledger holding it is unreachable, not because
        // anything releases it.
        //
        // The resetting path arrives here too, having checked and attached
        // nothing yet, and takes the same branch below.
        _ => None,
    };
    let attach = match attach {
        Some(attach) => attach,
        None => {
            // A ledger this call minted or reset has no passengers, so this cannot
            // refuse for the cadence rule. It can still lose to another upgrade
            // racing the same account, which is a refusal AFTER the eviction and
            // the one case the ordering above cannot reach - it needs two
            // upgrades interleaved inside this window, where the pre-claim check
            // needs none.
            if let Err(sitting) = account_state.try_sit(ticket.boat().key()) {
                let sitting_speed = sitting.speed();
                return (
                    StatusCode::BAD_REQUEST,
                    format!(
                        "account {account} is already seated on {symbol} at speed \
                         {sitting_speed}; a ledger carries one cadence",
                        account = account_state.account_id.as_str()
                    ),
                )
                    .into_response();
            }
            // Counted onto the account here instead, since the branch above
            // never ran. Either way the socket is counted on BEFORE the upgrade
            // completes and off it when its passenger drops, which is what keeps
            // the account attached across the gap before `bind_lanes` and what
            // freezes it if this upgrade is abandoned and never binds anything.
            state.run.attach(&account_state)
        }
    };
    // The ledger-side install, on the account the claim actually returned.
    state
        .run
        .register_instrument(&account_state, &profile)
        .await;
    let passenger = Passenger {
        symbol,
        ticket,
        duration_ms: query.duration_ms,
        account_state,
        presented_identity: query.callsign.clone(),
        attach: Some(attach),
        // Before the 101, not inside the spawned handler. See the field.
        alive: state.run.passenger_guard(),
    };
    ws.max_message_size(mogwai_protocol::MAX_INBOUND_MESSAGE_BYTES)
        .max_frame_size(mogwai_protocol::MAX_INBOUND_MESSAGE_BYTES)
        .on_upgrade(move |socket| handle_socket(socket, state, passenger))
}

fn send_admission(lanes: &ExecLanes, msg: VenueMessage) -> Result<(), CloseSpec> {
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
    cmd: Command,
    /// The process-wide command slot, deliberately NOT underscore-prefixed:
    /// the dispatcher drops it EXPLICITLY after the command has been acted on,
    /// so an edit that releases it early has to delete a visible `drop`, not
    /// merely rename a binding in a destructure pattern.
    global_slot: OwnedSemaphorePermit,
}

async fn dispatch_command(
    cmd: Command,
    state: &AppState,
    lanes: &ExecLanes,
    symbol: &mogwai_protocol::Symbol,
    boat: &Arc<crate::boatyard::Boat>,
    account_state: &Arc<crate::run::Account>,
) {
    let class = CommandClass::of(&cmd);
    match process_order_cmd(cmd, state, &state.run, lanes, symbol, boat, account_state).await {
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
            // consumer a fill for an order it was never told was accepted.
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
            let account = account_state.account_id.as_str();
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
    account_state: Arc<crate::run::Account>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // The permit's scope is the correctness property: the process-wide
        // slot may return only after the command has been ACTED ON, or the
        // bound counts acceptances rather than work in flight. The drop is
        // therefore EXPLICIT, sequenced after the awaited dispatch, rather
        // than left to a destructure binding whose lifetime an underscore
        // pattern would silently end early.
        while let Some(queued) = commands.recv().await {
            dispatch_command(queued.cmd, &state, &lanes, &symbol, &boat, &account_state).await;
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
    account_state: Arc<crate::run::Account>,
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
            let delay = account_state
                .delay_ms
                .load(std::sync::atomic::Ordering::Relaxed)
                .saturating_add(class.map_or(0, |class| account_state.ack_ms(class)));
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

/// The one task that owns this socket's sink, and therefore the only place that
/// knows what a consumer actually received.
///
/// IT READS THE BOAT DIRECTLY rather than being fed by a separate task, and that
/// is the whole reason the gap declaration below can be honest. A feed task can
/// only report what it took off the ring and handed on; whether those frames
/// were then suppressed by an armed window, lost to a failed write, or overtaken
/// by a close is knowledge that lives HERE. Splitting the two put the frontier
/// in one task and the truth in another, so a declaration composed upstream
/// could name a boundary the consumer never reached.
///
/// It also makes the two writes that must not be separated - a declaration and
/// the frame it precedes - ordinary sequential statements rather than a protocol
/// between tasks.
///
/// One consequence worth stating: execution output no longer shares a queue with
/// market data, so a fill can now overtake market frames still on the ring.
/// Their relative order was never a guarantee - the two producers raced on one
/// channel - and separate execution and market streams are what a real venue
/// gives a consumer anyway.
async fn run_writer(
    mut sink: futures_util::stream::SplitSink<WebSocket, Message>,
    mut prio_rx: mpsc::UnboundedReceiver<Outbound>,
    mut out_rx: mpsc::Receiver<Outbound>,
    mut tape: tokio::sync::broadcast::Receiver<TapeFrame>,
    snapshot: Option<TapeFrame>,
    account_state: Arc<crate::run::Account>,
    sim: mogwai_protocol::SimClock,
) {
    let mut writer = Writer {
        account_state,
        sim,
        delivered_market_ts: None,
        last_seen_market_ts: None,
        pending: None,
        episode: 0,
        skipped_total: 0,
    };
    let mut priority_open = true;
    let mut held_open = true;
    let mut tape_open = true;
    if let Some(frame) = snapshot
        && writer.write_market(&mut sink, frame).await.is_err()
    {
        return;
    }
    while priority_open || held_open {
        let event = tokio::select! {
            biased;
            message = prio_rx.recv(), if priority_open => match message {
                Some(message) => WriterEvent::Outbound(message),
                None => { priority_open = false; continue; }
            },
            message = out_rx.recv(), if held_open => match message {
                Some(message) => WriterEvent::Outbound(message),
                None => { held_open = false; continue; }
            },
            result = tape.recv(), if tape_open => WriterEvent::Tape(result),
        };
        match event {
            WriterEvent::Outbound(Outbound::Close(close)) => {
                // A close the VENUE chose, on a socket it can still write. The
                // owed declaration goes out ahead of it: the terminal frame
                // already reveals that the venue is alive, so stating a hole
                // first discloses nothing the close does not.
                drop(writer.settle_pending(&mut sink).await);
                drop(
                    sink.send(Message::Close(Some(axum::extract::ws::CloseFrame {
                        code: close.code,
                        reason: close.reason.into(),
                    })))
                    .await,
                );
                return;
            }
            WriterEvent::Outbound(Outbound::Frame(frame)) => {
                if writer.write_lane_frame(&mut sink, &frame).await.is_err() {
                    return;
                }
            }
            WriterEvent::Tape(Ok(frame)) => {
                if writer.write_market(&mut sink, frame).await.is_err() {
                    return;
                }
            }
            // The ring turned over before this passenger read it. Not a fault
            // and not a divergence: it is loss, and the only thing owed is that
            // the consumer be told where. Folded into any outstanding
            // declaration rather than announced here, because the boundary it
            // names has not arrived yet.
            WriterEvent::Tape(Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped))) => {
                writer.record_loss(skipped);
            }
            // The boat wound down. The socket may still be serving execution
            // output, so this is not a teardown - but an owed declaration will
            // never get the resumption boundary it was waiting for, so it is
            // stated now with the upper bound open.
            WriterEvent::Tape(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                tape_open = false;
                if writer.settle_pending(&mut sink).await.is_err() {
                    return;
                }
            }
        }
    }
}

/// What the writer is woken by. The tape is a third source rather than another
/// producer on the outbound lane, which is what keeps market delivery out of a
/// queue whose depth would be a second, unnamed buffer in front of the ring.
enum WriterEvent {
    Outbound(Outbound),
    Tape(Result<TapeFrame, tokio::sync::broadcast::error::RecvError>),
}

/// The sink is gone. Every write path returns this rather than breaking, so no
/// exit can skip the pending-declaration settlement by construction.
struct SinkGone;

struct Writer {
    account_state: Arc<crate::run::Account>,
    /// This socket's BOAT clock, which is the only clock its armed windows are
    /// judged on.
    sim: mogwai_protocol::SimClock,
    /// The last market frame that CROSSED THE SOCKET. Advanced only by a write
    /// that returned `Ok`, never by reading a frame, suppressing one, or
    /// queueing one - which is what makes it the consumer's frontier rather than
    /// the venue's.
    delivered_market_ts: Option<u64>,
    /// The last market frame READ, delivered or not. Separate from the frontier
    /// because the backward-time guard is about the tape's own ordering, which a
    /// suppressed frame still participates in.
    last_seen_market_ts: Option<u64>,
    pending: Option<PendingGap>,
    episode: u64,
    skipped_total: u64,
}

impl Writer {
    /// Is venue output being withheld from this account right now?
    ///
    /// THIS ACCOUNT'S windows, not the venue's: transport havoc corrupts what one
    /// connection receives, so arming a blackout on one account must not black
    /// out every other account on the exchange.
    fn suppressed(&self, class: FrameClass) -> bool {
        // A terminal announcement outranks every armed window: see `FrameClass`.
        if class == FrameClass::Terminal {
            return false;
        }
        let now = sim_now_ns(self.sim);
        // `GoDark` wholesale, `StallData` on market data alone.
        self.account_state.dark.open_at(self.sim, now)
            || (class == FrameClass::MarketData && self.account_state.stall.open_at(self.sim, now))
    }

    async fn send(
        &self,
        sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
        payload: &str,
    ) -> Result<(), SinkGone> {
        sink.send(Message::Text(payload.into()))
            .await
            .map_err(|_| SinkGone)
    }

    /// An execution, heartbeat or terminal frame from the outbound lanes.
    async fn write_lane_frame(
        &self,
        sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
        frame: &OutboundFrame,
    ) -> Result<(), SinkGone> {
        if self.suppressed(frame.class) {
            return Ok(());
        }
        self.send(sink, frame.payload.as_ref()).await
    }

    /// One market frame, and the declaration that must precede it if this
    /// passenger has an unstated hole.
    ///
    /// THE TWO WRITES ARE ONE STATEMENT. Nothing returns to the select between
    /// them, so no close, completion or execution frame can be interleaved
    /// between a declaration and the frame whose arrival it dates.
    async fn write_market(
        &mut self,
        sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
        frame: TapeFrame,
    ) -> Result<(), SinkGone> {
        if self
            .last_seen_market_ts
            .is_some_and(|prior| frame.ts_event < prior)
        {
            tracing::error!(
                prior_ts_event = self.last_seen_market_ts,
                frame_ts_event = frame.ts_event,
                "VENUE FAULT: tape feed moved backward in event time; killing the connection \
                 rather than silently ending market data"
            );
            let close = CloseSpec::venue_fault(format!(
                "venue fault: tape event time moved backward from {:?} to {}",
                self.last_seen_market_ts, frame.ts_event
            ));
            drop(
                sink.send(Message::Close(Some(axum::extract::ws::CloseFrame {
                    code: close.code,
                    reason: close.reason.into(),
                })))
                .await,
            );
            return Err(SinkGone);
        }
        self.last_seen_market_ts = Some(frame.ts_event);
        // A withheld frame is not loss and does not move the frontier: the
        // consumer armed this, and what it is owed is the silence it asked for.
        if self.suppressed(FrameClass::MarketData) {
            return Ok(());
        }
        if let Some(gap) = self.pending.take() {
            let declaration = self.declaration(&gap, Some(frame.ts_event));
            self.send(sink, &declaration).await?;
        }
        self.send(sink, frame.payload.as_ref()).await?;
        self.delivered_market_ts = Some(frame.ts_event);
        Ok(())
    }

    /// Fold loss into the outstanding declaration, or open one.
    ///
    /// The lower bound is taken from the DELIVERED frontier at the moment the
    /// first loss of an episode is recorded, so a hole that opened while an
    /// armed window was withholding frames still names the last instant this
    /// consumer actually saw.
    fn record_loss(&mut self, skipped: u64) {
        self.skipped_total = self.skipped_total.saturating_add(skipped);
        match &mut self.pending {
            Some(gap) => gap.skipped = gap.skipped.saturating_add(skipped),
            None => {
                self.pending = Some(PendingGap {
                    skipped,
                    after_ts_event: self.delivered_market_ts,
                });
            }
        }
    }

    /// State an owed declaration when no resumption boundary is coming.
    ///
    /// Called on tape closure and ahead of a venue-chosen close. It is a no-op
    /// when nothing is owed, and it declines to speak into an armed blackout -
    /// the frames the consumer is not receiving right now are the ones it asked
    /// not to receive.
    async fn settle_pending(
        &mut self,
        sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    ) -> Result<(), SinkGone> {
        if self.suppressed(FrameClass::MarketData) {
            return Ok(());
        }
        let Some(gap) = self.pending.take() else {
            return Ok(());
        };
        let declaration = self.declaration(&gap, None);
        self.send(sink, &declaration).await
    }

    fn declaration(&mut self, gap: &PendingGap, resumed_ts_event: Option<u64>) -> String {
        self.episode = self.episode.saturating_add(1);
        tracing::warn!(
            episode = self.episode,
            skipped = gap.skipped,
            skipped_total = self.skipped_total,
            after_ts_event = gap.after_ts_event,
            resumed_ts_event,
            "declaring a hole in this passenger's market view; the venue keeps serving"
        );
        serde_json::to_string(&VenueMessage::FeedLagged {
            episode: self.episode,
            skipped: gap.skipped,
            skipped_total: self.skipped_total,
            after_ts_event: gap.after_ts_event,
            resumed_ts_event,
        })
        .expect("FeedLagged serializes")
    }
}

async fn handle_socket(socket: WebSocket, state: AppState, mut passenger: Passenger) {
    tracing::info!(symbol = %passenger.symbol, "socket bound to river");
    // The passenger's claim on the process, `Passenger::alive`, is already
    // held, taken before the 101 rather than here. `passenger` lives to the end
    // of this function, past the writer's close flush, which is where the claim
    // is given up.
    let (sink, mut stream) = socket.split();
    let (out_tx, out_rx) = mpsc::channel(256);
    let (held_tx, held_rx) = mpsc::unbounded_channel();
    let (prio_tx, prio_rx) = mpsc::unbounded_channel();
    let lanes = ExecLanes::new(held_tx, prio_tx, build_admission_limits(&state.cfg));
    let (command_tx, command_rx) = mpsc::channel(state.cfg.pending_command_acts);
    let boat_sim = passenger.ticket.boat().sim;
    let dispatcher = spawn_command_dispatcher(
        command_rx,
        state.clone(),
        lanes.clone(),
        Arc::clone(&passenger.symbol),
        Arc::clone(passenger.ticket.boat()),
        Arc::clone(&passenger.account_state),
    );
    // This connection's own boat, and its own cursor on that boat's ring: a busy
    // river cannot lag a passenger subscribed to a quiet one, and loss here is a
    // property of the water this socket asked for.
    let (tape, snapshot) = passenger.ticket.boat().tape.subscribe_with_snapshot();
    let writer = tokio::spawn(run_writer(
        sink,
        prio_rx,
        out_rx,
        tape,
        snapshot,
        Arc::clone(&passenger.account_state),
        boat_sim,
    ));
    // Venue-ORIGINATED execution output (a trigger fill nobody commanded)
    // is delivered through these lanes, so the run has to know about them for
    // as long as this connection lives.
    let lane_id = state.run.bind_lanes(
        lanes.clone(),
        passenger.account_state.account_id.as_str(),
        passenger.presented_identity.as_deref(),
    );
    // ATTACHED from here, which is what un-freezes the account and puts it back
    // in the sweep. Bound AFTER the lane, so anything the resume retires has a
    // lane to be delivered on; a returning socket learns what its absence cost
    // rather than discovering a cancelled order by querying.
    let resumed = state
        .run
        .resume(
            &passenger.account_state,
            &passenger.symbol,
            crate::config::sim_now_ns(boat_sim),
        )
        .await;
    if !resumed.is_empty() {
        let shape = passenger.account_state.engine.lock().await.book_shape();
        if let Some(reservation) = lanes.reserve_swept(&shape, resumed.len(), resumed.len()) {
            drop(lanes.submit_produced(reservation, Instant::now(), None, resumed));
        }
    }
    let pump = spawn_exec_pump(
        held_rx,
        Arc::clone(&passenger.account_state),
        boat_sim,
        out_tx.clone(),
    );
    // Venue-originated liveness. Survives `StallData` (it is not market data)
    // but not `GoDark` (which gates the writer wholesale), which is exactly the
    // distinction it exists to make observable: a stalled feed and a dead venue
    // must not look the same to a consumer.
    let heartbeat = (state.cfg.venue_heartbeat_ms > 0).then(|| {
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
            .wall_duration(sim_duration_from_millis(beat_state.cfg.venue_heartbeat_ms))
            .max(MIN_HEARTBEAT_WALL);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(period).await;
                let frame = OutboundFrame {
                    payload: Arc::from(
                        serde_json::to_string(&VenueMessage::Heartbeat {
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
    // Where this passenger's own ride starts, on its boat's clock. Taken beside
    // the timer it anchors so the two cannot drift apart, and NOT derivable from
    // the boat: a boat placed for an earlier passenger has been running since
    // before this one existed, so its epoch is somebody else's boarding.
    let boarded_ns = sim_now_ns(boat_sim);
    let duration = passenger
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
                        serde_json::to_string(&VenueMessage::RunComplete {
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
        // Every other exit below needs the consumer to act - its close frame, its
        // EOF, or the run ending - so an evicted socket whose peer ignores the
        // close frame stayed here, and its `Passenger` kept the account riding
        // that boat. The passenger that evicted it was then refused
        // its own reconnect at a different speed. See `ExecLanes::closed`.
        let closed = lanes.closed();
        loop {
            tokio::select! {
                () = closed.notified() => break,
                changed = completion.changed() => {
                    let completed = if changed.is_ok() { *completion.borrow_and_update() } else { None };
                    if completed.is_some() {
                        let (sim_now_ns, elapsed_ns) = completion_on_boat_clock(boat_sim);
                        drop(out_tx.send(Outbound::Frame(OutboundFrame { payload: Arc::from(serde_json::to_string(&VenueMessage::RunComplete { sim_now_ns, elapsed_ns }).expect("RunComplete serializes")), class: FrameClass::Terminal, charge: None, slot: None })).await);
                        drop(out_tx.send(Outbound::Close(CloseSpec { code: mogwai_protocol::close::NORMAL, reason: mogwai_protocol::close::RUN_COMPLETE.into() })).await);
                    }
                    break;
                }
                () = async { if let Some(timer) = duration.as_mut().as_pin_mut() { timer.await } }, if duration.is_some() => {
                    // THE RUN WINS A TIE, and this re-read is what decides it.
                    // The completion watch and this timer are sibling select
                    // branches, so when both are ready the scheduler picks one -
                    // which was invisible while both announced the same frame
                    // and would become an externally visible claim about what
                    // ended the socket now that they do not. A run that has
                    // already reached its deadline is the stronger fact and the
                    // one that stays true for a consumer deciding whether to
                    // redial, so it is announced instead.
                    let (frame, reason) = if current_completion(&mut completion).is_some() {
                        let (sim_now_ns, elapsed_ns) = completion_on_boat_clock(boat_sim);
                        (VenueMessage::RunComplete { sim_now_ns, elapsed_ns }, mogwai_protocol::close::RUN_COMPLETE)
                    } else {
                        let now = sim_now_ns(boat_sim);
                        (VenueMessage::PassengerDurationComplete {
                            sim_now_ns: now,
                            // OBSERVED since this passenger boarded, not the
                            // deadline restated and not the boat's own span: a
                            // shared boat can predate its passenger, so the
                            // run-completion helper - which measures from the
                            // boat epoch - would report someone else's ride.
                            elapsed_ns: now.saturating_sub(boarded_ns),
                            declared_duration_ns: passenger.duration_ms.unwrap_or(0).saturating_mul(1_000_000),
                        }, mogwai_protocol::close::DURATION_COMPLETE)
                    };
                    drop(out_tx.send(Outbound::Frame(OutboundFrame { payload: Arc::from(serde_json::to_string(&frame).expect("a terminal announcement serializes")), class: FrameClass::Terminal, charge: None, slot: None })).await);
                    drop(out_tx.send(Outbound::Close(CloseSpec { code: mogwai_protocol::close::NORMAL, reason: reason.into() })).await);
                    break;
                }
                message = stream.next() => match message {
                    Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                    Some(Ok(Message::Text(text))) => match serde_json::from_str::<Command>(&text) {
                        Ok(command) => {
                            let subject = crate::http::admission_subject(&command);
                            let queued = Arc::clone(&state.pending_commands)
                                .try_acquire_owned()
                                .ok()
                                .map(|global_slot| QueuedCommand { cmd: command, global_slot });
                            if queued.is_none_or(|queued| command_tx.try_send(queued).is_err()) {
                                drop(send_admission(&lanes, VenueMessage::AdmissionRejected {
                                    subject,
                                    reason: "venue command capacity exhausted".into(),
                                    retryable: true,
                                    ts_event: sim_now_ns(boat_sim),
                                }));
                            }
                        }
                        Err(err) => { drop(send_admission(&lanes, VenueMessage::ProtocolError { reason: truncate_reason(format!("invalid command frame: {err}")), ts_event: sim_now_ns(boat_sim) })); }
                    },
                    Some(Ok(Message::Binary(_))) => {
                        drop(send_admission(&lanes, VenueMessage::ProtocolError {
                            reason: "binary command frames are unsupported; send JSON text".into(),
                            ts_event: sim_now_ns(boat_sim),
                        }));
                    }
                    Some(Ok(_)) => {}
                }
            }
        }
    }
    state.run.release_lanes(lane_id);
    // Given up here rather than with the passenger, which outlives this for as
    // long as the writer needs to flush its close frame: the account is no
    // longer being read the moment its lane is gone, and leaving it counted-in
    // for the writer's grace would keep it in the sweep for that long. The
    // passenger still carries the guard, so an abandoned upgrade that never got
    // here is covered by the drop.
    drop(passenger.attach.take());
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
    use super::{SocketQuery, current_completion};

    /// The identity parameter was `/ws?session=` until the callsign ruling, and
    /// the break is DESIGNED: a consumer carrying the old spelling must be told,
    /// not quietly served. `deny_unknown_fields` is the mechanism and this is
    /// what holds it - the same shape as
    /// `config::a_config_naming_the_retired_heartbeat_key_is_refused` on the
    /// operator side.
    ///
    /// Silently ignoring the old key would be the worst of the readings
    /// available: the socket would present NO identity, take the always-evict
    /// behaviour, and disconnect the very peer leg it was configured to coexist
    /// with - with nothing in the log saying why.
    ///
    /// Parsed through `Query`, which is the carrier the handler uses, so this
    /// cannot pass against a query string axum would reject or reject one it
    /// would take.
    #[test]
    fn the_retired_session_query_key_is_refused() {
        let parse = |query: &str| {
            let uri: axum::http::Uri = format!("http://venue/ws?{query}")
                .parse()
                .expect("a legal uri");
            axum::extract::Query::<SocketQuery>::try_from_uri(&uri)
        };
        let err = parse("account=WYRD-820&session=alpha")
            .expect_err("the retired identity key must be refused");
        let refusal = format!("{err:?}");
        assert!(
            refusal.contains("session"),
            "the refusal must name the key the consumer sent: {refusal}"
        );
        let accepted =
            parse("account=WYRD-820&callsign=alpha").expect("the current identity key parses");
        assert_eq!(accepted.0.callsign.as_deref(), Some("alpha"));
    }

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
