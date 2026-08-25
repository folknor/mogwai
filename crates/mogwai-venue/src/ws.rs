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
    /// The last market frame that crossed the socket before the loss. `None`
    /// when the loss preceded this passenger's first delivered frame, which is
    /// why it is not merely the last frame the venue read.
    after_ts_event: Option<u64>,
}

/// The upgrade's query string, exactly as the consumer wrote it.
///
/// `deny_unknown_fields` is a wire-compatibility decision, taken knowingly: a
/// consumer that sends a key this carrier does not handle is refused rather than
/// silently served a different river, speed or duration than it asked for. The
/// price is that any unrecognized key is a `400`, including
/// one an unrelated consumer, proxy or tracing layer appends, and including a
/// future key added before its handling lands. That is accepted:
/// accepted-and-ignored is the failure mode this carrier exists to prevent, and
/// the venue's consumers are its own. Relaxing it later is a wire change that owes
/// its own reasoning, not a tidy-up.
///
/// A repeated `symbol` key is not an error - `serde_urlencoded` keeps the last
/// occurrence - so the last one wins and is then validated like any other.
///
/// The identity key was `session` until the callsign ruling retired `session`
/// as a name for anything but the trading day. `deny_unknown_fields` is what
/// makes that break loud for a consumer still sending the old spelling: it is a
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
    /// `100.0000001` board the same boat. An unserved speed places a second
    /// boat on the same water rather than being refused - speed mutates no
    /// generated value, so it is a second cursor, not a second river. The one
    /// refusal left is per ledger: an account already riding this river at
    /// another speed would be judged on two clocks.
    ///
    /// Sharing at all only applies to the unnamed form, a preset plus a
    /// duration, the request that says "wherever you are is fine". A named
    /// window always gets its own river even against an identical request
    /// already running: the first requester is by then some sim-time ahead, and
    /// asking for a window means being served from its start.
    #[serde(default)]
    speed: Option<f64>,
    /// Absent means indefinite. Simulated milliseconds, measured on the boat's
    /// clock from this passenger's boarding instant and not from boot. A
    /// duration is a property of the passenger, so passengers with different
    /// durations still share one boat; each announces `RunComplete` and closes
    /// at its own deadline, and the boat winds down when the last one leaves.
    #[serde(default)]
    duration_ms: Option<u64>,
    /// The account to trade under. Absent means the venue's default account,
    /// which exists for the ephemeral single-consumer venue where naming one
    /// would be ceremony - it is not a venue-wide account every connection
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
    /// The generator arm this passenger's water carries, in four flat keys so
    /// the query string stays readable and `deny_unknown_fields` still covers
    /// them.
    ///
    /// This is the fork. A passenger carrying an arm boards a different river
    /// than one without it, rather than mutating water someone else may already
    /// be reading, so two accounts can run a clean strategy and a surged one on
    /// one exchange without either seeing the other's weather. It rides the
    /// upgrade rather than a control post because a posted default is run-wide
    /// state: on a shared venue that would let one consumer decide what every
    /// other account's next boarding resolves to.
    ///
    /// `surge_start_ms` is an offset from the run origin, not from this
    /// passenger's boarding instant. That is what lets two passengers share:
    /// "starting when I connect" names a different window for every boarding
    /// instant, so it would fork a river per connection and share nothing. The
    /// consequence to expect is that boarding late with a zero offset boards
    /// water whose surge is already over - the river had its weather whether or
    /// not anyone was aboard, which is what exogenous water means.
    ///
    /// Milliseconds, deliberately, where the identity underneath is
    /// nanoseconds. Two harness paths computing the same intended start through
    /// different units would otherwise differ by sub-millisecond residue and
    /// each strand a river of its own against a cap that never evicts.
    #[serde(default)]
    surge_start_ms: Option<u64>,
    #[serde(default)]
    surge_duration_ms: Option<u64>,
    #[serde(default)]
    surge_rate_mult: Option<f64>,
    #[serde(default)]
    surge_children_mult: Option<f64>,
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
    /// The identity this socket presented, so a later claim on the same account
    /// can tell related sockets from a stranger's.
    pub(crate) presented_identity: Option<String>,
    /// Whether this passenger's admission found its account unattended, and is
    /// therefore a return from a freeze rather than an additional socket on a
    /// live ledger.
    ///
    /// Sampled by the admission commit and carried here, because the commit is
    /// the only instant that can answer it: committing is what makes the account
    /// attended, so asking later always answers no. What rides on it is whether
    /// `resume` retires the book this account holds off the river being joined,
    /// which cancels resting orders and closes positions - not something an
    /// ordinary second socket of a live account may do.
    pub(crate) resumed_from_freeze: bool,
    /// This connection's registration, given up when the socket is done with it.
    ///
    /// An `Option` so `handle_socket` can give it up the instant it has released
    /// its lane, rather than at the end of its own teardown: the boat ticket
    /// above must outlive the writer's close frame, and the account must not be
    /// swept for that long.
    ///
    /// Dropping it removes the connection record, which gives up the ride, the
    /// lanes and this connection's share of attendance in one transition. There
    /// is no separate ride release any more - the old shape gave up the ride
    /// here, the lane somewhere else and an attach count in a third place, at
    /// three different moments, which is why each had to tolerate finding
    /// nothing. It drops on the abandoned-upgrade path too, where the handler
    /// never runs at all, so an upgrade that never reaches `handle_socket` still
    /// ends with nothing reading the account.
    pub(crate) attach: Option<crate::run::Attach>,
    /// This passenger's claim on the process, taken in `admit` before the 101 is
    /// returned and dropped with the passenger, after the writer has flushed its
    /// close.
    ///
    /// Taken in the admission rather than at the top of `handle_socket`,
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

impl Drop for Passenger {
    fn drop(&mut self) {
        // Release the registry ride before `ticket` can remove the last boat.
        //
        // This impl exists only to impose that order, and it is load-bearing
        // rather than defensive: `ticket` is declared above `attach`, so
        // without it Rust's declaration-order field drop would run the two the
        // wrong way round. `handle_socket` gives the ride up at the same
        // earlier boundary by hand; this arm covers an upgrade abandoned before
        // that handler ever runs.
        //
        // What the order buys is the argument that `BoatKey` needs no
        // per-placement nonce. A registry match can only name a boat while this
        // passenger still owns its ticket, and the ticket is what keeps that
        // boat placed - so while a stale ride could still be matched, no new
        // boat can have taken the key, and once the key is free there is no
        // ride left to match it. Reverse the order and both halves fail at
        // once: the boat winds down, a newcomer places a fresh one under the
        // identical `(river, speed)` key, and the ride still sitting here
        // matches it. That is precisely the case a nonce would have to
        // distinguish.
        //
        // The argument therefore rests on one condition and is invalidated by
        // any change that breaks it: no path may remove a boat without first
        // retiring every registry ride whose ticket holds it. A new teardown
        // route that drops a ticket while some ride survives owes either this
        // ordering or the nonce.
        drop(self.attach.take());
    }
}

/// Bind one socket to one river, or refuse before the 101.
///
/// A refusal is a status, not a close code: an unserved symbol on `/trades` is
/// a `400` naming what is served, and a WebSocket close after a successful
/// upgrade is the "looks like an outage" ambiguity `CLOSE_VENUE_FAULT` fights.
/// Returning here spawns no task, allocates no lane and opens no socket.
///
/// The extractor order is convention matching the other handlers, not a
/// constraint: all three are `FromRequestParts`, so any order compiles.
pub(crate) async fn ws_upgrade(
    ws: WebSocketUpgrade,
    Query(query): Query<SocketQuery>,
    State(state): State<AppState>,
) -> Response {
    let passenger = match admit(state.clone(), query).await {
        Ok(passenger) => passenger,
        Err(refusal) => return refusal,
    };
    ws.max_message_size(mogwai_protocol::MAX_INBOUND_MESSAGE_BYTES)
        .max_frame_size(mogwai_protocol::MAX_INBOUND_MESSAGE_BYTES)
        .on_upgrade(move |socket| handle_socket(socket, state, passenger))
}

/// Everything `/ws` decides, from the query to a fully admitted passenger, with
/// a refusal response as the error.
///
/// Split out of `ws_upgrade` so the whole admission - including the commit -
/// completes before the 101 is built, and so a test can hold the finished
/// passenger and ask the registry what the upgrade already established. The
/// handshake linearization the two-socket topology rests on is exactly the
/// property that split states: when this returns `Ok`, the connection is
/// committed.
///
/// The refusal is an `axum::Response` rather than a small error type because
/// every one of them is already a status and a sentence written for the
/// consumer, and there is exactly one per refused upgrade. Boxing it to satisfy
/// the size lint would buy an allocation on a path that is about to open a
/// socket or close a connection, and cost the refusals their one readable form.
#[expect(
    clippy::result_large_err,
    reason = "one Response per refused upgrade, off any hot path"
)]
async fn admit(state: AppState, query: SocketQuery) -> Result<Passenger, Response> {
    let symbol = match resolve_socket_symbol(query.symbol.as_deref(), &state.run.default_symbol) {
        Ok(symbol) => symbol,
        Err(body) => return Err((StatusCode::BAD_REQUEST, body).into_response()),
    };
    // Resolved before the instrument, because the instrument is registered on
    // this account's ledger. A malformed id is refused here rather than at
    // first order: nautilus cannot construct an `AccountId` from a bare word, so
    // a venue that accepted one would be refused by every consumer later.
    //
    // Claimed, not merely looked up: claiming an account evicts whoever already
    // holds it. Sockets presenting the same account and callsign coexist, while
    // a different or absent callsign is a new claim on the account.
    // Bounded and charset-checked before it is stored or compared: it arrives
    // in a URL, is echoed in no frame, and an unbounded one would be per-socket
    // memory a consumer sets for free.
    //
    // The account id is the client's, never minted per connection, and that is
    // load-bearing rather than incidental. From the venue's side a reconnect is
    // indistinguishable from a stranger claiming the id: a dropped socket the
    // adapter redialed, an armed `GoDark`, and a client process that died and
    // restarted against a still-running venue all look the same here. If the id
    // were born with the socket, the redial case would silently open a fresh
    // account with a reset balance and peak equity, so a hiccup would wipe a
    // run's profit and loss. A stable client-supplied id is what makes a
    // returning socket a continuation.
    if let Some(callsign) = query.callsign.as_deref()
        && let Err(reason) = mogwai_protocol::validate_callsign(callsign)
    {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("callsign is not usable: {reason}"),
        )
            .into_response());
    }
    let callsign = query.callsign.as_deref();
    // Nothing is claimed yet, and the order below is the whole point: every
    // refusal this handler can make is decided before `claim_account`, because claiming
    // closes the incumbent's sockets and, under `reset_account_on_reconnect`,
    // discards its ledger. Refusing after that turned any of these five 400s
    // into a one-request, unauthenticated way to disconnect a live consumer and
    // wipe its position book while never connecting at all -
    // `GET /ws?account=X&speed=NaN` was the cheapest spelling. Eviction is now
    // the last thing that happens before the 101.
    let (account_id, claimed) = match &query.account {
        Some(named) => match mogwai_protocol::AccountId::parse(named) {
            Ok(account_id) => (account_id, true),
            Err(error) => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    format!("account id is not usable: {error}"),
                )
                    .into_response());
            }
        },
        None => (state.run.default_account_id(), false),
    };
    // The bind-time shape refusal: an invalid resolved shape or a
    // funding-barred one is a configuration error, named here and before any
    // trading, rather than surfacing later as a fill-time funds rejection.
    //
    // Resolved, not yet registered. Resolution is a property of the venue and
    // is the only fallible half, so it answers here where nothing has been
    // taken from anybody; the ledger-side install happens on the account the
    // claim returns, further down.
    let profile = match state.rivers.resolve_profile(&symbol) {
        Ok(profile) => profile,
        Err(error) => return Err((StatusCode::BAD_REQUEST, error.to_string()).into_response()),
    };
    // This account's funding, against the shape it is binding.
    //
    // The boot-time barred set answers the same question for the venue's
    // configured `[balances]`, which is now only what an unnamed account opens
    // with - a consumer that named its own balances is funded in whatever it said,
    // and the venue has no way to know at boot what that will be. So the check
    // moves here for named accounts, where it is still knowable with no order at
    // all and is still a configuration error rather than a trading outcome.
    //
    // Presence, never sufficiency: running out is depletion, and a funds
    // rejection on a served shape must keep meaning that and only that.
    //
    // Asked of the ledger this connection will get: a claim that resets serves
    // the configured opening balances rather than whatever the account holds
    // now.
    let settlement = profile.def.class.settlement_currency();
    // Which ledger the checks below are taken against, sampled before the first
    // of them reads it and carried into the reservation, which refuses if this
    // account's ledger has been replaced in between. See the ledger identity
    // boundary in `crate::registry`: sampling it at the reservation instead
    // would name the ledger that exists after the reads rather than the one
    // they saw, which is a check that cannot fail.
    let observed_incarnation = state.run.ledger_incarnation(&account_id);
    let resetting = state
        .run
        .claim_discards_ledger(&account_id, claimed, callsign);
    if let Some(reset) = state.run.daily_reset_minute(&account_id, resetting)
        && let Some(reason) = daily_reset_refusal(
            account_id.as_str(),
            &symbol,
            profile.calendar.as_ref(),
            reset,
        )
    {
        return Err((StatusCode::BAD_REQUEST, reason).into_response());
    }
    if !state
        .run
        .funded_in(&account_id, resetting, settlement)
        .await
    {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "account {account} is not funded in {settlement}, which is what {symbol} settles \
                 in; open the account with a {settlement} balance",
                account = account_id.as_str()
            ),
        )
            .into_response());
    }
    let speed = query.speed.unwrap_or(state.cfg.speed);
    if !speed.is_finite() || speed < 0.0 {
        return Err((
            StatusCode::BAD_REQUEST,
            "speed must be finite and non-negative",
        )
            .into_response());
    }
    // Resolved here, once, and then carried by the boat, the ticket and the
    // passenger: no serving path re-derives it, so none of them can reach water
    // this passenger is not on. Refused before the account is claimed, so a
    // malformed arm cannot evict an incumbent on its way to a 400.
    let arm = match mogwai_protocol::control::GeneratorArm::normalize(
        query.surge_start_ms.unwrap_or(0).saturating_mul(1_000_000),
        query.surge_duration_ms.unwrap_or(0),
        query.surge_rate_mult.unwrap_or(1.0),
        query.surge_children_mult.unwrap_or(1.0),
    ) {
        Ok(arm) => arm,
        Err(reason) => return Err((StatusCode::BAD_REQUEST, reason).into_response()),
    };
    let river = state.rivers.resolve_key(&profile, arm);
    // Phase one of the admission, and the reason the rest of this handler can
    // be written as ordinary fallible code. The reservation takes exclusive
    // authority over this account without claiming anything and without
    // touching the incumbent, so every step below may refuse or be cancelled
    // and the live consumer pays nothing. Dropping it rolls the reservation
    // back.
    //
    // The cadence rule is decided here rather than after the placement, which
    // is what makes it a check the incumbent cannot lose to. The seat is
    // knowable now: the river is resolved above and the speed quantizes without
    // a boat.
    let speed_micros = match crate::boatyard::quantize_speed(speed) {
        Ok(speed_micros) => speed_micros,
        Err(error) => return Err((StatusCode::BAD_REQUEST, error.to_string()).into_response()),
    };
    let seat = crate::registry::Seat {
        river: river.clone(),
        speed_micros,
    };
    let mut reservation = match state.run.reserve_admission(
        account_id.as_str(),
        seat,
        resetting,
        observed_incarnation,
    ) {
        Ok(reservation) => reservation,
        Err(crate::registry::AdmissionRefusal::LedgerMoved) => {
            return Err((
                StatusCode::CONFLICT,
                format!(
                    "account {account} was reset while this upgrade was being decided, so its \
                     funding and policy were checked against a ledger that no longer exists; retry",
                    account = account_id.as_str()
                ),
            )
                .into_response());
        }
        Err(crate::registry::AdmissionRefusal::CadenceConflict(held)) => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!(
                    "account {account} is already seated on {symbol} at speed {held_speed}; \
                     {marker}",
                    account = account_id.as_str(),
                    held_speed = held.speed(),
                    marker = mogwai_protocol::control::CADENCE_CONFLICT_MARKER,
                ),
            )
                .into_response());
        }
        Err(crate::registry::AdmissionRefusal::Busy) => {
            return Err((
                StatusCode::CONFLICT,
                format!(
                    "account {account} has another connection being admitted; retry",
                    account = account_id.as_str()
                ),
            )
                .into_response());
        }
    };
    let ticket = match state
        .run
        .boatyard
        .board(&BoardRequest { river, speed })
        .await
    {
        Ok(ticket) => ticket,
        // Placement performs the river's first materialization, so what failed
        // decides whose fault it is, and the two answers are not
        // interchangeable. A river cap is a reachable, intentional refusal of
        // what was asked for, and stays a 400. A validated shape whose generator
        // could not produce it is the venue failing a promise it already made -
        // there is nothing the caller can change, so reporting it as a bad
        // request sends a consumer to fix a request that was never wrong, and
        // leaves it retrying forever against a venue that will never serve it.
        //
        // The venue-fault arm latches, which takes the run down the terminal
        // path and puts the fault on `/health`. Without that the venue announced
        // a healthy readiness line, refused every upgrade, and went on reporting
        // itself sound.
        Err(BoardRefusal::Placement(failure)) => {
            if failure.venue_fault {
                state
                    .run
                    .latch_materialize_fault(symbol.as_ref(), &failure.message);
                return Err((
                    StatusCode::SERVICE_UNAVAILABLE,
                    format!(
                        "the venue could not materialize {symbol}, which its own config \
                         validated: {failure}. This run is faulted; see GET /health"
                    ),
                )
                    .into_response());
            }
            return Err((
                StatusCode::BAD_REQUEST,
                format!("could not place boat for {symbol}: {failure}"),
            )
                .into_response());
        }
    };
    // Taken before the tail below, and therefore before the 101. See the field.
    let alive = state.run.passenger_guard();
    let callsign = query.callsign.clone();
    let duration_ms = query.duration_ms;
    // Kept behind, because the id itself moves into the tail and the two
    // failure answers below have to name the account they could not admit.
    let account_id_for_refusal = account_id.as_str().to_owned();
    // Phase three, and how it runs matters as much as what it does. Two
    // properties have to hold at once and each one alone is a defect.
    //
    // It runs in a task of its own, because the work must be owned by the task
    // that does it. Hyper drops this handler's future when the client goes
    // away, and the tail evicts the incumbent, replaces a ledger and installs a
    // connection; a future cancelled midway through that leaves an account
    // claimed by nobody. A spawned task cannot be cancelled by the caller
    // disappearing: it runs to completion, and the `Passenger` it yields is
    // dropped by the runtime when nobody takes it, which releases the attach,
    // the ticket and the liveness guard in the order `Passenger::drop` fixes.
    //
    // And it is awaited before the upgrade response is built, because the 101
    // is the consumer's only proof that its admission committed. The supported
    // two-socket shared-callsign topology depends on that: a client opening its
    // second leg the instant the first handshake completes would otherwise find
    // its own account still reserved and be answered a `409` by a connection it
    // had already been told was admitted. Returning the 101 with the tail still
    // pending is exactly that regression.
    let tail = tokio::spawn(async move {
        // The ledger side, and it happens before the commit deliberately. The
        // reservation is still outstanding here, so no other admission can
        // reserve across the replacement; the commit then advances the ledger
        // identity in the same exclusive window, with no await between the two.
        // Together those make the incarnation a real boundary rather than a
        // check that agrees with its own mutation - see the module docs in
        // `crate::registry`. Run this after the commit instead and a second
        // socket can read the outgoing ledger, reserve the identity the commit
        // already advanced to, and be admitted on checks nothing supports.
        //
        // Nothing below refuses, which is what makes the ordering safe: every
        // fallible step is behind us, so the ledger cannot be discarded for a
        // connection that then fails to arrive.
        //
        // `resetting` was evaluated once, above, and is handed to
        // `claim_account` rather than re-derived, so this cannot decide to
        // reset an account the funding check was taken against on the
        // assumption that it would not.
        let account_state =
            state
                .run
                .claim_account(&account_id, claimed, callsign.as_deref(), resetting);
        // The sole linearization point of the whole upgrade. Every fallible
        // step is behind us: the shape resolved, the account funded, the speed
        // quantized, the arm normalized and the boat placed. So the commit
        // installs this connection, selects whoever it displaces and takes the
        // continuity handoff in one transaction under the registry lock.
        //
        // Nothing refuses after this point, which is the property the old
        // ordering could not have. It evicted and then re-ran a cadence check
        // against a ledger the eviction had just minted, so an upgrade losing
        // that race refused a consumer whose incumbent it had already closed.
        // There is no window here for that: the reservation held this account
        // exclusively from before the placement, so no other upgrade can have
        // moved the ledger underneath us.
        //
        // The one thing it can answer that is not a success is the registry's
        // own invariant failing, which a live reservation forbids. It is
        // answered rather than papered over: a `Committed` that installed
        // nothing would give this passenger an `Attach` for a connection the
        // registry never had, lanes nothing delivers to and a ride no sweeper
        // can find. The claim above has already run at that point, so a
        // resetting admission leaves a fresh ledger with nobody on it - which
        // is the correct outcome for an admission that did not happen, and
        // still better than a socket that looks admitted and receives nothing.
        let (attach, committed) = state.run.commit_admission(
            &mut reservation,
            callsign.as_deref(),
            Some(ticket.boat().key()),
            claimed,
        )?;
        // Outside the registry lock, deliberately: closing a lane sends on a
        // channel, and the commit hands the displaced set out rather than
        // closing it, so a send can never block while the registry mutex is
        // held.
        let displaced = state
            .run
            .close_displaced(account_id.as_str(), &committed.displaced);
        if displaced > 0 {
            tracing::info!(
                account = %account_id.as_str(),
                displaced,
                reset = resetting,
                "a new connection claimed an existing account",
            );
        }
        // The ledger-side install, on the account the claim actually returned.
        state
            .run
            .register_instrument(&account_state, &profile)
            .await;
        Some(Passenger {
            symbol,
            ticket,
            duration_ms,
            account_state,
            presented_identity: callsign,
            resumed_from_freeze: committed.resumed_from_freeze,
            attach: Some(attach),
            // Before the 101, not inside the spawned handler. See the field.
            alive,
        })
    });
    match tail.await {
        Ok(Some(passenger)) => Ok(passenger),
        Ok(None) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            format!("account {account_id_for_refusal} could not be installed by the venue; retry"),
        )
            .into_response()),
        // The tail panicked. Everything it held was dropped as the task
        // unwound - the reservation rolled back, or the connection released by
        // its own guard - so refusing here strands nothing, and no 101 is owed
        // for an admission that did not finish.
        Err(panicked) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("the venue could not finish admitting this connection: {panicked}"),
        )
            .into_response()),
    }
}

fn daily_reset_refusal(
    account: &str,
    symbol: &str,
    calendar: Option<&mogwai_data::SessionCalendar>,
    reset: u32,
) -> Option<String> {
    calendar
        .is_some_and(|calendar| !calendar.contains_utc_minute_of_day(reset))
        .then(|| {
            format!(
                "account {account} resets its daily loss limit at UTC minute {reset}, which the {symbol} footprint never contains"
            )
        })
}

#[cfg(test)]
mod reset_tests {
    use super::*;

    #[test]
    fn a_daily_policy_is_refused_when_its_footprint_omits_the_reset() {
        let calendar = mogwai_data::SessionCalendar {
            utc_offset_minutes: 0,
            open_windows: vec![mogwai_data::WeeklyWindow {
                start_minute: 1_200,
                end_minute: 1_380,
            }],
            settlement_minute_of_day: None,
        };
        let refusal = daily_reset_refusal("WYRD-1", "ASIA", Some(&calendar), 1_020)
            .expect("the absent boundary must refuse");
        assert!(refusal.contains("WYRD-1") && refusal.contains("ASIA"));
        assert!(daily_reset_refusal("WYRD-1", "ASIA", None, 1_020).is_none());
    }
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
    /// The process-wide command slot, deliberately not underscore-prefixed:
    /// the dispatcher drops it explicitly after the command has been acted on,
    /// so an edit that releases it early has to delete a visible `drop`, not
    /// merely rename a binding in a destructure pattern.
    global_slot: OwnedSemaphorePermit,
}

/// One history page in flight per connection.
///
/// The global synthesis slots and their waiter queue already bound the venue,
/// but they bound it across connections: without a per-connection cap one
/// passenger can hold every slot and every waiter, and the command queue's own
/// bound does not help because a queued history command is cheap to accept and
/// expensive to serve. One is the right number because pagination is a pull -
/// a consumer has no use for a second page before it has taken the first.
const HISTORY_PAGES_IN_FLIGHT: usize = 1;

async fn dispatch_command(
    cmd: Command,
    state: &AppState,
    lanes: &ExecLanes,
    symbol: &mogwai_protocol::Symbol,
    boat: &Arc<crate::boatyard::Boat>,
    account_state: &Arc<crate::run::Account>,
    history_permits: &Arc<tokio::sync::Semaphore>,
) {
    // Intercepted before the order path, and spawned rather than awaited. This
    // dispatcher is sequential per connection, which is what stops a cancel
    // from overtaking the submit it cancels - and that same sequencing would
    // put a multi-thousand-row generator walk in front of every cancel, modify,
    // reconciliation query and heartbeat behind it. The connection would then
    // fill its command queue and close for what is really history contention.
    if let Command::QueryHistory { .. } = cmd {
        spawn_history_page(cmd, state, lanes, boat, history_permits);
        return;
    }
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
            // Do not put an `.await` between here and `submit_produced`, and the
            // reason is not style. Teardown aborts this dispatcher, so a yield
            // here could discard the local events after the engine mutation.
            // `process_order_cmd` released the engine lock
            // before returning, so the order below is already visible to the
            // sweeper while its `OrderAccepted` is still unpublished. Publication
            // order is enqueue order and nothing else: `submit_produced` appends
            // in call order, and the exec pump is one sequential task that sleeps
            // in-line, so it is head-of-line and cannot reorder. A sweep that
            // enqueued a fill for this order first would therefore hand the
            // consumer a fill for an order it was never told was accepted.
            //
            // That does not happen today, and the protection is timing rather
            // than design. The sweeper must gather `pending_scans`, walk the
            // tape - the dominant cost in the system - re-lock and apply before
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

/// Serve one history page off the dispatcher, on a task of its own.
///
/// The permit discipline is the correctness property, and it is the one the
/// HTTP handlers already pay for: a dropped connection drops the awaiting
/// future, but a running blocking task cannot be cancelled, so a permit held by
/// the future would be released while the synthesis it covers was still
/// resident. The global synthesis slot is therefore acquired inside the spawned
/// task and moved into the blocking closure, and the per-connection permit
/// travels with it. Both are given up by dropping, wherever that turns out to
/// happen - which on every path is after the work ends.
fn spawn_history_page(
    cmd: Command,
    state: &AppState,
    lanes: &ExecLanes,
    boat: &Arc<crate::boatyard::Boat>,
    history_permits: &Arc<tokio::sync::Semaphore>,
) {
    let Command::QueryHistory {
        request_id,
        kind,
        start,
        end,
        continuation,
    } = cmd
    else {
        return;
    };
    let reject = |lanes: &ExecLanes, reason: String, retryable: bool| {
        drop(send_admission(
            lanes,
            VenueMessage::HistoryRejected {
                request_id: request_id.clone(),
                reason,
                retryable,
            },
        ));
    };
    // Refused rather than queued when this connection already has a page in
    // flight, and correlated so the consumer can resolve the request it just
    // made. Silently queueing would let a consumer that ignores its own
    // pagination discipline accumulate work the venue then has to finish.
    let Ok(connection_permit) = Arc::clone(history_permits).try_acquire_owned() else {
        reject(
            lanes,
            "this connection already has a history page in flight".to_owned(),
            true,
        );
        return;
    };
    // The passenger's own river, taken from its boat. This is the line the
    // whole decision is about: nothing here consults a symbol, so nothing here
    // can name the wrong river.
    let key = boat.key().river().clone();
    let rivers = Arc::clone(&state.rivers);
    let slots = Arc::clone(&state.history_slots);
    let waiters = Arc::clone(&state.history_slot_waiters);
    // The tighter of the two presents. The run clock bounds any caller against
    // the venue's own present; the boat clock bounds this passenger against its
    // own. On a paced run at speed 1 they are nearly the same and the second
    // buys nothing visible, which is exactly why it was missing: on an unpaced
    // or slow-boat run the boat trails the run clock, and serving the span
    // between them would hand this passenger water it has not been delivered.
    let present = state.run_now().min(sim_now_ns(boat.sim));
    let run_start_ns = state.run.started_ns;
    let lanes = lanes.clone();
    let panic_request_id = request_id.clone();
    tokio::spawn(async move {
        let Ok(synthesis_slot) = crate::http::acquire_history_slot(&slots, &waiters).await else {
            drop(send_admission(
                &lanes,
                VenueMessage::HistoryRejected {
                    request_id,
                    reason: "venue history capacity exhausted".to_owned(),
                    retryable: true,
                },
            ));
            return;
        };
        let page = tokio::task::spawn_blocking(move || {
            // Both permits move in here and die here, after the walk and the
            // serialization they cover.
            let _synthesis_slot = synthesis_slot;
            let _connection_permit = connection_permit;
            crate::history::serve_page(
                &rivers,
                &crate::history::PageRequest {
                    key: &key,
                    kind,
                    start,
                    end,
                    continuation: continuation.as_deref(),
                    present,
                    run_start_ns,
                },
            )
            .map(|page| {
                serde_json::to_string(&VenueMessage::HistoryPage {
                    request_id: request_id.clone(),
                    kind,
                    rows: page.rows,
                    cutoff: page.cutoff,
                    continuation: page.continuation,
                    complete: page.complete,
                })
            })
            .map_err(|refusal| (request_id, refusal))
        })
        .await;
        finish_history_page(page, &lanes, panic_request_id);
    });
}

type HistoryJoin = Result<
    Result<Result<String, serde_json::Error>, (String, crate::history::Refusal)>,
    tokio::task::JoinError,
>;

/// Resolve one history request, however the synthesis ended.
///
/// Every arm but one resolves the consumer's `request_id`, and the exception is
/// stated where it sits: a saturated lane means the peer is not reading, so
/// there is nothing to send a refusal on either.
///
/// `panic_request_id` is a clone taken before the id moves into the blocking
/// closure, because a panicked task hands back no id of its own. Split out of
/// `spawn_history_page` so a panicked synthesis can be exercised at all - a
/// blocking task that panics cannot be provoked through the socket path.
fn finish_history_page(page: HistoryJoin, lanes: &ExecLanes, panic_request_id: String) {
    match page {
        Ok(Ok(Ok(payload))) => {
            let Some(slot) = lanes.reserve_admission() else {
                // The lane is saturated, which means the peer is not
                // reading. Nothing useful can be sent, including a refusal.
                return;
            };
            drop(lanes.emit_history(slot, payload));
        }
        // Unreachable by construction - every field is a number, a string
        // or a plain enum - and logged rather than given a wire meaning for
        // the same reason the other producers do it.
        Ok(Ok(Err(error))) => {
            tracing::error!(%error, "could not serialize a history page");
        }
        Ok(Err((request_id, refusal))) => {
            drop(send_admission(
                lanes,
                VenueMessage::HistoryRejected {
                    request_id,
                    reason: refusal.reason,
                    retryable: refusal.retryable,
                },
            ));
        }
        // A panicked synthesis is a venue fault, not an empty window, and
        // the consumer is told so rather than left waiting out its timeout.
        Err(error) => {
            tracing::error!(%error, "history synthesis task failed");
            drop(send_admission(
                lanes,
                VenueMessage::HistoryRejected {
                    request_id: panic_request_id,
                    reason: format!("venue history synthesis failed: {error}"),
                    retryable: true,
                },
            ));
        }
    }
}

#[cfg(test)]
mod history_panic_tests {
    use super::*;
    use crate::admission::Outbound;

    #[tokio::test]
    async fn panicked_history_synthesis_resolves_the_request_as_retryable() {
        let page = tokio::task::spawn_blocking(
            || -> Result<Result<String, serde_json::Error>, (String, crate::history::Refusal)> {
                panic!("synthesis witness")
            },
        )
        .await;
        let (lanes, mut receivers) = ExecLanes::detached();
        finish_history_page(page, &lanes, "history-panic-1".to_owned());

        let Outbound::Frame(frame) = receivers.prio_rx.try_recv().expect("a correlated refusal")
        else {
            panic!("history panic produced a close instead of a refusal")
        };
        let message: VenueMessage = serde_json::from_str(&frame.payload).expect("wire frame");
        let VenueMessage::HistoryRejected {
            request_id,
            retryable,
            reason,
        } = message
        else {
            panic!("history panic produced the wrong frame: {message:?}")
        };
        assert_eq!(request_id, "history-panic-1");
        assert!(retryable, "a venue synthesis fault can be retried");
        assert!(
            reason.contains("synthesis witness"),
            "the refusal diagnoses the task failure"
        );
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
    // Per connection, minted here so it lives exactly as long as the dispatcher
    // that hands pages out.
    let history_permits = Arc::new(tokio::sync::Semaphore::new(HISTORY_PAGES_IN_FLIGHT));
    tokio::spawn(async move {
        // The permit's scope is the correctness property: the process-wide
        // slot may return only after the command has been acted on, or the
        // bound counts acceptances rather than work in flight. The drop is
        // therefore explicit, sequenced after the awaited dispatch, rather
        // than left to a destructure binding whose lifetime an underscore
        // pattern would silently end early.
        //
        // A history command is the one that does not finish inside the
        // dispatch: it is handed to a task of its own, so the global command
        // slot is released once the request has been accepted rather than once
        // the page has been produced. That is deliberate, and it is why the
        // page has bounds of its own - a per-connection permit and the global
        // synthesis slots - rather than leaning on this one.
        while let Some(queued) = commands.recv().await {
            dispatch_command(
                queued.cmd,
                &state,
                &lanes,
                &symbol,
                &boat,
                &account_state,
                &history_permits,
            )
            .await;
            drop(queued.global_slot);
        }
    })
}

/// Re-stamp the venue's completion on this socket's clock.
///
/// The venue deadline is a venue property and is measured on the venue clock -
/// there is no boat to ask when the last passenger has left. But the frame goes
/// to a socket whose every other stamp is its boat's, so the venue instant is
/// the signal and this is the conversion. `elapsed_ns` is how much tape this
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
/// It reads the boat directly rather than being fed by a separate task, and that
/// is the whole reason the gap declaration below can be honest. A feed task can
/// only report what it took off the ring and handed on; whether those frames
/// were then suppressed by an armed window, lost to a failed write, or overtaken
/// by a close is knowledge that lives here. Splitting the two put the frontier
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
                // A close the venue chose, on a socket it can still write. The
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
    /// This socket's boat clock, which is the only clock its armed windows are
    /// judged on.
    sim: mogwai_protocol::SimClock,
    /// The last market frame that crossed the socket. Advanced only by a write
    /// that returned `Ok`, never by reading a frame, suppressing one, or
    /// queueing one - which is what makes it the consumer's frontier rather than
    /// the venue's.
    delivered_market_ts: Option<u64>,
    /// The last market frame read, delivered or not. Separate from the frontier
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
    /// This account's windows, not the venue's: transport havoc corrupts what one
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
    /// The two writes are one statement. Nothing returns to the select between
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
    /// The lower bound is taken from the delivered frontier at the moment the
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
    // The id comes from the admission that produced this passenger, so the
    // connection record the registry filed and the lanes that deliver for it
    // name the same connection.
    let lanes = ExecLanes::new(
        passenger
            .attach
            .as_ref()
            .expect("a passenger reaching its handler still holds its admission")
            .connection_id(),
        held_tx,
        prio_tx,
        build_admission_limits(&state.cfg),
    );
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
    // Venue-originated execution output (a trigger fill nobody commanded)
    // is delivered through these lanes, so the run has to know about them for
    // as long as this connection lives.
    let lane_id = state.run.bind_lanes(
        lanes.clone(),
        passenger.account_state.account_id.as_str(),
        passenger.presented_identity.as_deref(),
    );
    // Reading from here, which is what puts the account back in the sweep and
    // retires the continuity handoff this connection has held since its
    // admission committed. Run after the lane is bound, so anything the resume
    // retires has a lane to be delivered on: a returning socket learns what its
    // absence cost rather than discovering a cancelled order by querying.
    let resumed = state
        .run
        .resume(
            &passenger.account_state,
            &passenger.symbol,
            crate::config::sim_now_ns(boat_sim),
            passenger.resumed_from_freeze,
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
        // Floored in wall time, for the same reason `MIN_SWEEP_WALL` exists on
        // the sweep side and with the same 5 ms. The configured period is
        // simulated, so `wall_duration` shrinks it linearly with `speed` while
        // the cost of a beat - a serialization, a channel send, a writer wake -
        // does not, and `wall_duration`'s own floor is one nanosecond. At a high
        // speed the heartbeat task therefore degenerated into a
        // timer-granularity loop pushing uncharged frames into a 256-slot
        // channel the peer has to read. Liveness needs a frame now and then, not
        // a frame per timer tick, so the floor costs the signal nothing.
        //
        // Its own constant, not the sweeper's, though the number matches
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
    // the timer it anchors so the two cannot drift apart, and not derivable from
    // the boat: a boat placed for an earlier passenger has been running since
    // before this one existed, so its epoch is somebody else's boarding.
    let boarded_ns = sim_now_ns(boat_sim);
    let duration = passenger
        .duration_ms
        .map(|ms| tokio::time::sleep(boat_sim.wall_duration(sim_duration_from_millis(ms))));
    tokio::pin!(duration);
    // The venue's own `(sim_now_ns, elapsed_ns)` pair is deliberately discarded
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
        // The venue's own close ends this loop, without waiting on the peer.
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
                    // The run wins a tie, and this re-read is what decides it.
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
                            // Observed since this passenger boarded, not the
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
    state
        .run
        .release_lanes(passenger.account_state.account_id.as_str(), lane_id);
    // Given up here rather than with the passenger, which outlives this for as
    // long as the writer needs to flush its close frame: the account stops being
    // read the moment its connection record is gone, and holding it for the
    // writer's grace would keep it in the sweep for that long. The passenger
    // still carries the guard, so an abandoned upgrade that never got here is
    // covered by the drop instead - and the release is idempotent, so the two
    // paths overlapping costs nothing.
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
    /// the break is designed: a consumer carrying the old spelling must be told,
    /// not quietly served. `deny_unknown_fields` is the mechanism and this is
    /// what holds it - the same shape as
    /// `config::a_config_naming_the_retired_heartbeat_key_is_refused` on the
    /// operator side.
    ///
    /// Silently ignoring the old key would be the worst of the readings
    /// available: the socket would present no identity, take the always-evict
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

    /// The differential is the point: a receiver subscribed after the terminal
    /// transition never sees a change, so a socket that only awaits
    /// `changed()` waits forever on a run that is already over. Both halves are
    /// asserted here, because asserting only the second would pass against the
    /// buggy shape too.
    #[tokio::test]
    async fn receiver_created_after_completion_observes_terminal_state() {
        let run = crate::run::tests::run(1_000, 400, None);
        // There are deliberately no receivers at the transition. This is the
        // production call site, not a hand-built channel whose send primitive
        // could drift from `Run::complete` again.
        run.complete(123, 45);
        let mut late = run.completion();

        let mut awaiting = run.completion();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(200), awaiting.changed(),)
                .await
                .is_err(),
            "a late receiver cannot reach the terminal state by waiting for a change"
        );

        assert_eq!(current_completion(&mut late), Some((123, 45)));
    }

    /// A venue over two configured shapes, funded in both their settlement
    /// currencies, so a second passenger can board a river the first is not on.
    ///
    /// The boot shape is taken by name rather than off `instrument_defs`, which
    /// walks a `HashMap`: with two shapes configured, the default symbol would
    /// otherwise be a coin flip.
    fn admission_state() -> crate::http::AppState {
        let profiles = std::sync::Arc::new(crate::source::InstrumentProfiles::from_profiles(vec![
            crate::config::profile_for_symbol("BTCUSDT").expect("BTCUSDT preset must resolve"),
            crate::config::profile_for_symbol("MNQ").expect("MNQ preset must resolve"),
        ]));
        let instrument = profiles
            .configured("BTCUSDT")
            .expect("the boot shape was just configured")
            .def
            .clone();
        let balances = ["USDT", "USD"]
            .into_iter()
            .map(|currency| {
                (
                    currency.to_owned(),
                    rust_decimal::Decimal::from(1_000_000_u32),
                )
            })
            .collect();
        let run = crate::run::Run::new(
            instrument,
            crate::source::Rivers::new(
                crate::source::TapeIdentity {
                    seeds: mogwai_protocol::RunSeeds::from_run_seed(42),
                    regime: None,
                },
                1_000,
                profiles,
            ),
            balances,
            mogwai_protocol::SimClock::identity(),
            1_000,
            400,
            None,
            mogwai_protocol::RunSeeds::from_run_seed(42),
            8,
            mogwai_protocol::OmsType::Netting,
            200,
            mogwai_protocol::AccountId::parse(crate::config::DEFAULT_ACCOUNT_ID)
                .expect("the default account id is legal"),
            false,
            std::collections::HashMap::new(),
            std::sync::mpsc::channel().0,
        );
        let rivers = std::sync::Arc::clone(&run.rivers);
        crate::http::AppState {
            run,
            cfg: crate::config::Config::default(),
            rivers,
            pending_commands: std::sync::Arc::new(tokio::sync::Semaphore::new(8)),
            history_slots: std::sync::Arc::new(tokio::sync::Semaphore::new(4)),
            history_slot_waiters: std::sync::Arc::new(tokio::sync::Semaphore::new(4)),
        }
    }

    fn socket_query(query: &str) -> SocketQuery {
        let uri: axum::http::Uri = format!("http://venue/ws?{query}")
            .parse()
            .expect("a legal uri");
        axum::extract::Query::<SocketQuery>::try_from_uri(&uri)
            .expect("the query must parse")
            .0
    }

    /// The handshake is a linearization point: when `/ws` has an upgrade
    /// response to give, that connection's admission has already committed.
    ///
    /// What rides on it is the supported two-socket shared-callsign topology. A
    /// consumer that has received the 101 for its data leg may open its
    /// execution leg in the very next instruction, and that second request must
    /// not find its own account still reserved and be answered `409 Conflict`
    /// by an admission it was already told had succeeded.
    ///
    /// The interleaving is forced by hand rather than threaded, because a
    /// threaded version is a coin flip against its own defect: `admit`
    /// returning IS the instant the 101 becomes available, so calling it again
    /// on the same account right here is the second leg arriving before
    /// anything else could possibly run. Detach the admission tail instead of
    /// awaiting it - the shape this test exists to forbid - and the second call
    /// is refused `Busy` every single time.
    #[tokio::test]
    async fn a_second_leg_is_not_refused_by_the_first_legs_own_admission() {
        let state = admission_state();
        let first = super::admit(
            state.clone(),
            socket_query("account=WYRD-820&callsign=alpha&symbol=BTCUSDT&speed=1"),
        )
        .await;
        assert!(
            first.is_ok(),
            "the first leg must be admitted before this test asks anything"
        );

        let second = super::admit(
            state.clone(),
            socket_query("account=WYRD-820&callsign=alpha&symbol=MNQ&speed=1"),
        )
        .await;
        if let Err(refusal) = second {
            let status = refusal.status();
            let body = axum::body::to_bytes(refusal.into_body(), 8192)
                .await
                .expect("a refusal carries a readable body");
            panic!(
                "the second leg of a shared callsign was refused {status}: {}",
                String::from_utf8_lossy(&body)
            );
        }
        drop(first);
    }
}
