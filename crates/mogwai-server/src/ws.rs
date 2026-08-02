// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The `/ws` gateway: one task per connection multiplexing order entry,
//! per-symbol market-data replay, the execution-delay pump, and the optional
//! heartbeat onto a single outbound channel. Everything here exists to serve
//! that one live socket; the plain request/response routes live in `http.rs`.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use axum::{
    extract::{
        Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use mogwai_data::TickEvent;
use mogwai_protocol::{
    ACCOUNT_QUERY_PARAM, AccountId, ClientMessage, CommandClass, MarketRegime, ServerMessage,
    SimClock, SubscriptionIssue, SubscriptionOutcome,
};
use tokio::sync::{Notify, mpsc};

use crate::accounts::{AccountSlot, SessionLease};
use crate::admission::{
    CLOSE_GRACE, CloseSpec, ExecLanes, HeldFrame, Outbound, OutboundFrame, Ticket,
};
use crate::config::{build_admission_limits, now_ns, sim_duration_from_millis, sim_now_ns};
use crate::http::{
    ActDelay, AppState, OrderOutcome, boundary_error, process_order_cmd,
    strip_unfireable_reopen_gap, validate_regime_or_clean,
};
use crate::source;
use crate::tape::{CursorState, RegimeKey, TapeKey, TapeLease, TapeSpawn};

pub(crate) async fn ws_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(params): Query<AccountParams>,
) -> Result<impl IntoResponse, (axum::http::StatusCode, String)> {
    let raw = params.account.ok_or_else(|| {
        (
            axum::http::StatusCode::BAD_REQUEST,
            format!("missing {ACCOUNT_QUERY_PARAM}"),
        )
    })?;
    let id = AccountId::parse(&raw).map_err(|err| {
        (
            axum::http::StatusCode::BAD_REQUEST,
            format!("invalid {ACCOUNT_QUERY_PARAM}: {err}"),
        )
    })?;
    let slot = state
        .accounts
        .acquire(&id, crate::config::now_ns())
        .map_err(|_| {
            (
                axum::http::StatusCode::TOO_MANY_REQUESTS,
                "account capacity exhausted".into(),
            )
        })?;
    Ok(ws.on_upgrade(move |socket| handle_socket(socket, state, slot)))
}

#[derive(serde::Deserialize)]
pub(crate) struct AccountParams {
    account: Option<String>,
}

/// Queue one admission frame on the PRIORITY lane, reserving its queue slot
/// first. Never blocks and never awaits, so it is safe on the read loop.
///
/// `Err` is the overload condition: the lane is full (the peer is not reading)
/// or the writer is gone. The caller must then stop reading and close with a
/// reason - the one thing it must NOT do is drop the refusal silently, which is
/// the failure mode this whole path exists to prevent. The writer's `GoDark`
/// gate still applies downstream; `DelayAcks` deliberately does not, because
/// admission truth is not engine output.
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

/// Emit an untargeted diagnostic on the priority lane. `Err` means the lane is
/// saturated, which is the overload condition: the caller must stop reading and
/// close, never silently drop the diagnostic.
fn send_exec_protocol_error(
    lanes: &ExecLanes,
    ts_event: u64,
    reason: String,
) -> Result<(), CloseSpec> {
    send_admission(lanes, ServerMessage::ProtocolError { reason, ts_event })
}

/// Reconcile ONE `Subscribe` entry's `start_ts` against the tape bounds,
/// mirroring the `/trades` handler's refusals but as a DEGRADATION - the live
/// feed still starts, because a subscribe carries the client's real want (the
/// feed itself). PURE: it returns the effective start plus the issue to report,
/// and the caller accumulates issues across entries and emits ONE coalesced
/// frame per `Subscribe`. Emitting here would cost one priority frame per entry,
/// which a 256-entry subscribe would turn into a deterministic overload close.
/// Build one `SubscriptionIssues` entry list from a subscribe's accumulated
/// outcomes: REFUSALS FIRST, then degradations, truncated at
/// `MAX_SUBSCRIPTION_ISSUES_LISTED`.
///
/// The ordering is what pays for keeping a lossy cap. The `if entries.len() <
/// CAP` guard the old refusal list used cannot do this on its own - it fills in
/// discovery order, so 16 clamped starts discovered before one unknown symbol
/// would truncate away the only outcome that killed a feed. A degradation lost
/// to the cap costs a log line; a refusal lost to the cap costs a feed.
pub(crate) fn coalesce_issues(
    mut refusals: Vec<SubscriptionOutcome>,
    degradations: Vec<SubscriptionOutcome>,
) -> Vec<SubscriptionOutcome> {
    refusals.extend(degradations);
    refusals.truncate(mogwai_protocol::MAX_SUBSCRIPTION_ISSUES_LISTED);
    refusals
}

pub(crate) fn reconcile_entry_start_ts(
    start_ts: Option<u64>,
    data_origin_ns: u64,
    sim_now: u64,
) -> (Option<u64>, Option<SubscriptionIssue>) {
    let Some(ts) = start_ts else {
        return (None, None);
    };
    // Below the tape origin: `/trades` refuses the identical window with a 422,
    // but here the stream is kept and the generator anchors at the origin
    // naturally (its first tick lands at or after the origin). This CLAMPS to the
    // origin rather than passing `start_ts` through, so the reported
    // `effective_start_ts` is genuinely the position the venue used and a client
    // may safely adopt it as a resume cursor. Observationally equivalent for the
    // generator, whose first tick already lands at or after the origin.
    if ts < data_origin_ns {
        return (
            Some(data_origin_ns),
            Some(SubscriptionIssue::StartBeforeOrigin {
                effective_start_ts: data_origin_ns,
            }),
        );
    }
    // Beyond sim-now: the WS twin of the `/trades` future refusal (F8). Honoring
    // it as given would extend the shared index into the future and, in identity
    // mode, emit an unpaced look-ahead first tick stamped ahead of the clock -
    // exactly what `/trades` 422s. Consistent with the below-origin degradation
    // above, clamp to a fresh live stream from the clock: returning `None` makes
    // the replay seek sim-now and seed its pacer there, so the first tick is
    // paced like any live subscribe, and resumes past a quiesced predecessor
    // rather than jumping to the future. Announce the clamp.
    if ts > sim_now {
        return (None, Some(SubscriptionIssue::StartAfterSimNow { sim_now }));
    }
    (start_ts, None)
}

/// The single writer task: the one owner of the socket's sink.
///
/// Two inputs. `prio_rx` is the priority lane, drained FIRST by the biased
/// select so an admission refusal overtakes queued market data and queued
/// already-delayed execution output; `held_rx` (here `rx`) is everything the
/// venue paces normally. Frames arrive pre-serialized from their producers,
/// which is what makes the byte budget charge real bytes and keeps JSON cost
/// off this task.
///
/// Generic over the sink so the lane-closure behavior below is reachable from a
/// test without a real socket; production passes the split `WebSocket` half.
pub(crate) async fn run_writer<S>(
    mut sink: S,
    mut prio_rx: mpsc::UnboundedReceiver<Outbound>,
    mut rx: mpsc::Receiver<Outbound>,
    sim: SimClock,
    slot: Arc<AccountSlot>,
) where
    S: SinkExt<Message> + Unpin,
{
    // The per-branch disable flags are load-bearing, not style. A bare
    // `select!` over both receivers busy-spins the moment ONE side closes:
    // a closed receiver returns `None` immediately and forever, so with the
    // priority lane still alive and `tx` dropped the loop would burn a core
    // until teardown - never yielding, so on a single-threaded runtime it
    // starves every other task on the same thread. "Both receivers returning
    // None ends the loop" is a property these flags create.
    let mut prio_open = true;
    let mut held_open = true;
    while prio_open || held_open {
        let outbound = tokio::select! {
            biased;
            msg = prio_rx.recv(), if prio_open => match msg {
                Some(v) => v,
                None => { prio_open = false; continue; }
            },
            msg = rx.recv(), if held_open => match msg {
                Some(v) => v,
                None => { held_open = false; continue; }
            },
        };
        let frame = match outbound {
            Outbound::Frame(frame) => frame,
            // The writer owns the sink, so it is the only task that can
            // state a reason on the way out.
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
        };
        // Both havoc gates drop the frame, which releases its held-lane
        // charge and its priority slot through `OutboundFrame`'s Drop - the
        // one honest exception to "produced execution truth is delivered",
        // pre-existing and documented in reference/havoc.md.
        let now = sim_now_ns(sim);
        if now < slot.dark_until_ns.load(Ordering::Relaxed) {
            continue;
        }
        if frame.is_market_data && now < slot.stall_until_ns.load(Ordering::Relaxed) {
            continue;
        }
        if sink
            .send(Message::Text(frame.payload.as_ref().into()))
            .await
            .is_err()
        {
            break; // client gone
        }
    }
}

/// One client session.
///
/// The socket is split so order events and replayed market data can be written
/// concurrently: every outbound [`ServerMessage`] funnels through one mpsc
/// channel drained by a single writer task. Order commands are processed inline
/// against the engine; `Subscribe` spawns a replay feeding the same channel.
/// Execution events (the engine's own output) take one extra hop through a
/// dedicated delay pump before landing in that channel, so an armed
/// `DelayAcks` window paces only execution traffic - see `spawn_exec_pump`
/// for why that hop exists and for the per-event delay contract.
async fn handle_socket(socket: WebSocket, state: AppState, slot: Arc<AccountSlot>) {
    let (sink, mut stream) = socket.split();
    let (tx, rx) = mpsc::channel::<Outbound>(1024);
    let (prio_tx, prio_rx) = mpsc::unbounded_channel::<Outbound>();
    let (held_tx, held_rx) = mpsc::unbounded_channel::<HeldFrame>();
    // Budget sizes come from the run config, whose defaults ARE the constants in
    // `admission`. They are configurable so this connection's refusal behavior
    // is reachable: at the shipped 8 MiB held budget and 64 queued priority
    // frames, driving a real socket into a refused reservation or a saturated
    // lane takes megabytes of engine output and a TCP window's worth of timing
    // luck, so the invariants would have no end-to-end gate at all.
    let lanes = ExecLanes::new(held_tx, prio_tx, build_admission_limits(&state.cfg));
    let lease = SessionLease::acquire(Arc::clone(&slot), crate::config::now_ns(), lanes.clone());
    // One replay stream PER SYMBOL, not per sorted symbol-set. Keying per set let
    // overlapping subscriptions (`[A,B]` then `[B,C]`) spawn two independent
    // streams both emitting `B` from independent generators/clocks, so the client
    // saw duplicated, interleaved, out-of-order-per-symbol `B` trades - breaking
    // the ascending-`ts_event` ordering the adapter's `PollCursor` relies on
    // (E.5). With a per-symbol map a given symbol is fed by exactly one stream:
    // re-subscribing a symbol already in flight quiesces (cancels + joins) the old
    // stream before the replacement emits, so no stale tick interleaves at the
    // seam (E.6); the handles are tracked and reaped so threads cannot pile up
    // under connect/subscribe/disconnect churn (E.7).
    let mut subscriptions: HashMap<String, Subscription> = HashMap::new();
    let mut generations: HashMap<String, u64> = HashMap::new();
    let sim = state.sim;

    // `tx`/`rx` carries everything the writer paces normally: market data
    // straight from the fanout tasks and the heartbeat, and execution output
    // after it has cleared the `DelayAcks` shift in `spawn_exec_pump` (the hop
    // exists so a delay sleep never blocks the writer's own recv loop). The
    // priority lane is the second input, drained FIRST by the biased select
    // below, so an admission refusal overtakes queued market data and queued
    // already-delayed execution output. Frames arrive pre-serialized: the
    // producers render them, which is what makes the byte budget charge real
    // bytes and keeps JSON cost off this single task.
    let writer = tokio::spawn(run_writer(sink, prio_rx, rx, sim, Arc::clone(&slot)));

    let exec_pump = spawn_exec_pump(held_rx, Arc::clone(&slot), state.sim, tx.clone());

    let heartbeat = if state.cfg.server_heartbeat_ms > 0 {
        Some(spawn_heartbeat(
            state.cfg.server_heartbeat_ms,
            state.sim,
            tx.clone(),
        ))
    } else {
        None
    };

    // The read loop breaks with the reason it stopped: `None` for an ordinary
    // disconnect, `Some(close)` when the priority lane could not take a
    // refusal. Every producer below reserves or refuses; none of them awaits a
    // full channel, which is what keeps the reader alive under an armed
    // `DelayAcks` no matter how deep the held backlog gets.
    // Teardown of the bound account must reach this session even if it never
    // makes another engine access - a market-data-only socket makes none, so
    // lazy discovery would leave it trading a phantom and pinning the removed
    // engine's retention (`seen_client_order_ids` / `closed` / `fills`) for the
    // daemon's life. Created once and polled by the select below, so the
    // registration survives across iterations.
    let teardown = slot.wait_for_teardown();
    tokio::pin!(teardown);

    let overload: Option<CloseSpec> = loop {
        let msg = tokio::select! {
            biased;
            () = &mut teardown => {
                tracing::info!(account = %slot.id.as_str(), "closing session: account destroyed");
                // On the priority lane, not the exec lane: teardown is transport
                // truth, and an armed DelayAcks on the doomed account must not
                // hold the notice behind it.
                drop(send_admission(
                    &lanes,
                    ServerMessage::ProtocolError {
                        reason: "account destroyed by the venue control plane".into(),
                        ts_event: sim_now_ns(state.sim),
                    },
                ));
                break None;
            }
            msg = stream.next() => msg,
        };
        let Some(Ok(msg)) = msg else {
            break None;
        };
        let Message::Text(text) = msg else { continue };

        let client_msg: ClientMessage = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(%e, %text, "undecodable client message");
                // Surface the decode failure on the wire instead of dropping it
                // silently: previously a malformed/under-specified frame (e.g.
                // `{"type":"Subscribe"}` missing `symbols`) looked identical to a
                // healthy-but-idle feed. It is the purest transport truth there
                // is - the venue could not even decode the frame - so it goes
                // out unattributed on the PRIORITY lane, ahead of held traffic
                // and exempt from `DelayAcks`: a client whose frames do not
                // decode learns so promptly instead of after an armed window.
                // `emit_admission` truncates serde's error text, which echoes
                // client-controlled field names.
                if let Err(close) = send_admission(
                    &lanes,
                    ServerMessage::AdmissionRejected {
                        subject: mogwai_protocol::AdmissionSubject::Frame,
                        reason: e.to_string(),
                        ts_event: sim_now_ns(state.sim),
                    },
                ) {
                    break Some(close);
                }
                continue;
            }
        };

        match client_msg {
            ClientMessage::Subscribe {
                subscriptions: entries_request,
            } => {
                // Cardinality and per-symbol length are capped at the boundary
                // (a malformed request, answered with the untargeted
                // diagnostic) so one read-loop iteration cannot be made to
                // demand 100k per-symbol reservations, and so a symbol that
                // reaches the wire is bounded by `MAX_SYMBOL_LEN` - which is
                // what makes the coalesced refusal frame's ceiling provable.
                if let Err(reason) = mogwai_protocol::validate_subscriptions(&entries_request) {
                    tracing::warn!(reason, count = entries_request.len(), "refusing subscribe");
                    if let Err(close) = send_exec_protocol_error(
                        &lanes,
                        sim_now_ns(state.sim),
                        format!("subscribe refused: {reason}"),
                    ) {
                        break Some(close);
                    }
                    continue;
                }
                // Per-entry outcomes COALESCE into ONE priority frame at the end
                // of this subscribe: a 256-entry subscribe of unknown symbols
                // must cost ONE queued frame, not 256, or it would
                // deterministically close a connection that `S22a` promises
                // stays up. Accumulated in two vecs so the frame can list
                // REFUSALS FIRST when the fixed cap truncates - a degradation
                // lost to the cap costs a log line, a refusal lost to the cap
                // costs a feed. `issues_total` and `refusals_total` carry the
                // true counts either way, so a client seeing more refusals than
                // it can read knows some feed it asked for is dead.
                let mut refusals: Vec<SubscriptionOutcome> = Vec::new();
                let mut degradations: Vec<SubscriptionOutcome> = Vec::new();
                let mut issues_total = 0usize;
                let mut refusals_total = 0usize;
                let mut overload_close = None;
                for entry in entries_request {
                    let symbol = entry.symbol;
                    // Unknown symbols get their diagnostic here rather than from a
                    // spawned fanout task: whether the venue lists a symbol is a
                    // cheap synchronous check, so there is no reason to spin up a
                    // tape thread that immediately poisons and leaves a dead
                    // entry in `subscriptions` (S22). The dead-SEEK diagnostic -
                    // knowable only by actually running the positioning seek -
                    // stays where it is discovered, on the tape thread.
                    //
                    // Note this arm `continue`s BEFORE the quiesce and before
                    // the tape attach, so a refused symbol never consumes a
                    // promise ticket either.
                    if state.profiles.get(&symbol).is_none() {
                        tracing::warn!(%symbol, "subscribe for unknown symbol streams nothing");
                        issues_total += 1;
                        refusals_total += 1;
                        refusals.push(SubscriptionOutcome {
                            generation: entry.generation,
                            symbol,
                            issue: SubscriptionIssue::UnknownSymbol,
                        });
                        continue;
                    }
                    // Monotonicity is enforced against a connection-lifetime
                    // high-water map, not against `subscriptions`: an Unsubscribe, or
                    // a quiesce followed by a capacity refusal, removes the
                    // stream, and enforcing off `subscriptions` would then accept a
                    // REUSED generation and let a delayed diagnostic about an
                    // old stream look current. No recorded generation means any
                    // generation is acceptable, so a first subscribe on a fresh
                    // connection never trips this. A refused entry leaves any
                    // running replay strictly alone - destroying a healthy
                    // stream over a client-ordering fault would be worse.
                    if let Some(&current) = generations.get(&symbol)
                        && current >= entry.generation
                    {
                        tracing::warn!(
                            %symbol,
                            generation = entry.generation,
                            current,
                            "subscribe generation is not ahead of the current one; refusing the entry"
                        );
                        issues_total += 1;
                        refusals_total += 1;
                        refusals.push(SubscriptionOutcome {
                            generation: entry.generation,
                            symbol,
                            issue: SubscriptionIssue::StaleGeneration { current },
                        });
                        continue;
                    }
                    // Record the generation IMMEDIATELY, before this entry's
                    // outcome is known, so a capacity refusal or a dead seek
                    // still burns it and a later reuse of it is refused.
                    generations.insert(symbol.clone(), entry.generation);
                    // An out-of-range regime is a DEGRADATION, not a refusal:
                    // the clean tape still streams. It used to be dropped in
                    // silence, which contradicts the per-entry truth this frame
                    // promises.
                    let (mut regime, invalid_regime) = validate_regime_or_clean(entry.regime);
                    if invalid_regime {
                        issues_total += 1;
                        degradations.push(SubscriptionOutcome {
                            generation: entry.generation,
                            symbol: symbol.clone(),
                            issue: SubscriptionIssue::InvalidRegime,
                        });
                    }
                    if let Some(at_ts) =
                        strip_unfireable_reopen_gap(&mut regime, state.data_origin_ns)
                    {
                        issues_total += 1;
                        degradations.push(SubscriptionOutcome {
                            generation: entry.generation,
                            symbol: symbol.clone(),
                            issue: SubscriptionIssue::ReopenGapUnfireable { at_ts },
                        });
                    }
                    let (start_ts, issue) = reconcile_entry_start_ts(
                        entry.start_ts,
                        state.data_origin_ns,
                        sim_now_ns(state.sim),
                    );
                    if let Some(issue) = issue {
                        match issue {
                            SubscriptionIssue::StartBeforeOrigin { effective_start_ts } => {
                                tracing::warn!(
                                    %symbol,
                                    generation = entry.generation,
                                    start_ts = entry.start_ts,
                                    data_origin = effective_start_ts,
                                    "subscribe start_ts precedes the tape origin; clamping to the origin"
                                );
                            }
                            SubscriptionIssue::StartAfterSimNow { sim_now } => {
                                tracing::warn!(
                                    %symbol,
                                    generation = entry.generation,
                                    start_ts = entry.start_ts,
                                    sim_now,
                                    "subscribe start_ts exceeds sim-now; clamping to a live stream from the clock"
                                );
                            }
                            other => tracing::warn!(
                                %symbol,
                                generation = entry.generation,
                                ?other,
                                "subscribe start_ts reconciliation reported an unexpected issue"
                            ),
                        }
                        issues_total += 1;
                        degradations.push(SubscriptionOutcome {
                            generation: entry.generation,
                            symbol: symbol.clone(),
                            issue,
                        });
                    }
                    // Quiesce any in-flight stream for this symbol BEFORE the
                    // replacement emits: cancel the old thread and join it (off
                    // the async worker) so it cannot land one last tick into the
                    // shared channel after the new generator starts. Without the
                    // join the old thread - blocked in a send or mid-`next_tick`
                    // - could deliver an out-of-order/duplicate tick at the seam
                    // (E.6). The join alone only stops the old thread from
                    // enqueuing anything ELSE, though - whatever it already
                    // enqueued before the cancel lands stays in the shared
                    // channel. `resume_floor` carries that thread's last
                    // successfully-sent `ts_event` (if it sent one) forward, so
                    // the replacement's own seek starts strictly past it instead
                    // of re-sampling a `sim_now` that could land at-or-before an
                    // already-delivered tick. The floor is read only AFTER the
                    // join (see `quiesce_and_resume_floor` for why the order is
                    // load-bearing).
                    //
                    // Capacity for this replay's ONE possible diagnostic (the
                    // dead-seek) is reserved BEFORE the quiesce, and from the
                    // separate promise pool rather than the 64-slot queue: a
                    // promise is held for the replay's whole life, so drawn
                    // from queue depth 64 healthy replays would leave no room
                    // for any actual refusal. Reserving after the quiesce would
                    // mean the old stream is already destroyed when the refusal
                    // is discovered - the client would have lost a live feed to
                    // a capacity problem it was never told about.
                    let Some(diag_ticket) = lanes.reserve_promise() else {
                        overload_close = Some(CloseSpec::overload(
                            "subscribe diagnostic capacity exhausted",
                        ));
                        break;
                    };
                    let resume_floor = if let Some(old) = subscriptions.remove(&symbol) {
                        quiesce_and_resume_floor(old).await
                    } else {
                        None
                    };
                    // The per-connection subscription cap, counted over the
                    // entries LIVE in this connection's map. Enforced AFTER the
                    // quiesce above, which is what makes a resubscribe free: the
                    // predecessor has already left the map, so a resubscribe at
                    // the cap replaces rather than being refused by its own
                    // predecessor. Nothing else bounds a connection's
                    // subscription count now that the replay-thread pool counts
                    // tapes rather than subscriptions. `0` is unbounded.
                    if state.cfg.max_subscriptions_per_connection != 0
                        && subscriptions.len() >= state.cfg.max_subscriptions_per_connection
                    {
                        tracing::warn!(
                            %symbol,
                            cap = state.cfg.max_subscriptions_per_connection,
                            "connection subscription cap reached; subscribe refused"
                        );
                        issues_total += 1;
                        refusals_total += 1;
                        refusals.push(SubscriptionOutcome {
                            generation: entry.generation,
                            symbol,
                            issue: SubscriptionIssue::ReplayCapacity,
                        });
                        continue;
                    }
                    // Ration the global tape pool (S22a): every DISTINCT tape is
                    // a dedicated OS thread, so without a ceiling a fleet of
                    // connections arming distinct regimes across the whole
                    // catalog exhausts the process thread limit. Subscribers
                    // sharing an existing tape never consume a permit. Attach
                    // AFTER the quiesce above so a resubscribe of THIS symbol -
                    // which just released the predecessor's lease - reclaims the
                    // permit here rather than deadlocking against its own
                    // cap-of-one. A cap of 0 sizes the pool at MAX_PERMITS, so
                    // this branch never trips when the cap is disabled. On
                    // exhaustion, refuse this symbol with a per-entry
                    // `ReplayCapacity` outcome - the wire name is unchanged
                    // because from the client's side the meaning is unchanged
                    // ("the venue's replay pool is full, nothing streams") - and
                    // leave the running streams untouched.
                    let key = TapeKey {
                        symbol: symbol.clone(),
                        data_origin_ns: state.data_origin_ns,
                        regime: RegimeKey::from_regime(regime.as_ref()),
                    };
                    let tape_spawn = TapeSpawn {
                        profiles: Arc::clone(&state.profiles),
                        regime,
                        sim: state.sim,
                        speed: state.cfg.speed,
                        gap_cap_ms: state.cfg.gap_cap_ms,
                        fanout_depth: state.cfg.fanout_depth,
                        zero_speed_stall_ms: state.cfg.zero_speed_stall_ms,
                    };
                    let lease = match state.tapes.attach(key, tape_spawn) {
                        Ok(lease) => lease,
                        Err(_) => {
                            tracing::warn!(
                                %symbol,
                                cap = state.cfg.max_concurrent_tapes,
                                "tape capacity reached; subscribe refused"
                            );
                            issues_total += 1;
                            refusals_total += 1;
                            refusals.push(SubscriptionOutcome {
                                generation: entry.generation,
                                symbol,
                                issue: SubscriptionIssue::ReplayCapacity,
                            });
                            continue;
                        }
                    };
                    let cancel = Arc::new(AtomicBool::new(false));
                    let cancel_wake = Arc::new(Notify::new());
                    let last_sent_ts = Arc::new(AtomicU64::new(NO_TICK_SENT));
                    let task = spawn_fanout(FanoutSpawn {
                        symbol: symbol.clone(),
                        generation: entry.generation,
                        lease,
                        target: resume_seek_target(start_ts, resume_floor),
                        regime,
                        profiles: Arc::clone(&state.profiles),
                        data_origin: state.data_origin_ns,
                        tx: tx.clone(),
                        lanes: lanes.clone(),
                        diag_ticket: Some(diag_ticket),
                        cancel: Arc::clone(&cancel),
                        cancel_wake: Arc::clone(&cancel_wake),
                        last_sent_ts: Arc::clone(&last_sent_ts),
                        sim: state.sim,
                        speed: state.cfg.speed,
                        gap_cap_ms: state.cfg.gap_cap_ms,
                    });
                    subscriptions.insert(
                        symbol,
                        Subscription {
                            cancel,
                            cancel_wake,
                            task,
                            last_sent_ts,
                        },
                    );
                }
                if issues_total > 0 {
                    let entries = coalesce_issues(refusals, degradations);
                    if let Err(close) = send_admission(
                        &lanes,
                        ServerMessage::SubscriptionIssues {
                            entries,
                            issues_total,
                            refusals_total,
                            ts_event: sim_now_ns(state.sim),
                        },
                    ) {
                        break Some(close);
                    }
                }
                if let Some(close) = overload_close {
                    break Some(close);
                }
            }
            ClientMessage::Unsubscribe { symbols } => {
                // Same cardinality/length cap as `Subscribe`: an unsubscribe
                // naming 100k symbols would make one read-loop iteration do
                // 100k map lookups and joins.
                if let Err(reason) = mogwai_protocol::validate_symbols(&symbols) {
                    tracing::warn!(reason, count = symbols.len(), "refusing unsubscribe");
                    if let Err(close) = send_exec_protocol_error(
                        &lanes,
                        sim_now_ns(state.sim),
                        format!("unsubscribe refused: {reason}"),
                    ) {
                        break Some(close);
                    }
                    continue;
                }
                for symbol in dedup_symbols(symbols) {
                    if let Some(old) = subscriptions.remove(&symbol) {
                        quiesce_subscription(old).await;
                    }
                }
            }
            order_cmd => {
                let class = CommandClass::of(&order_cmd);
                let act_ms = class.map_or(0, |class| lease.slot().act_ms(class));
                // A malformed command is refused by the PROTOCOL BOUNDARY, which
                // is not a venue act: it must be equally prompt on both carriers
                // and must not burn a pending-act slot. `boundary_error` takes no
                // reservation and touches no account, so consulting it here is
                // free, and `process_order_cmd` still runs the real boundary gate
                // on whichever path is taken.
                if act_ms > 0 && boundary_error(&order_cmd).is_none() {
                    // Bounded twice: a per-connection ticket so one client cannot
                    // spawn unbounded pending tasks under an armed hour-long act
                    // delay, and a process-wide permit so the whole box cannot be
                    // flooded across connections and carriers. Either refusal is
                    // visible and the engine never sees the command.
                    let Some(ticket) = lanes.reserve_act() else {
                        if let Err(close) = send_admission(
                            &lanes,
                            ServerMessage::AdmissionRejected {
                                subject: crate::http::admission_subject(&order_cmd),
                                reason: "pending command-latency queue saturated".into(),
                                ts_event: sim_now_ns(state.sim),
                            },
                        ) {
                            break Some(close);
                        }
                        continue;
                    };
                    let Ok(permit) = Arc::clone(&state.pending_acts).try_acquire_owned() else {
                        drop(ticket);
                        if let Err(close) = send_admission(
                            &lanes,
                            ServerMessage::AdmissionRejected {
                                subject: crate::http::admission_subject(&order_cmd),
                                reason: "venue pending-act capacity exhausted".into(),
                                ts_event: sim_now_ns(state.sim),
                            },
                        ) {
                            break Some(close);
                        }
                        continue;
                    };
                    // A pending act OUTLIVES the socket that sent it, so it is
                    // spawned bare rather than onto anything the connection owns:
                    // teardown neither awaits nor aborts it. The command is past
                    // the protocol boundary, so the venue has RECEIVED it, and
                    // dropping it on disconnect would model request loss - a
                    // different divergence mogwai already has. The mutation
                    // therefore lands and only the acknowledgment is discarded,
                    // because `lanes` is closed by then. Two existing facts make
                    // that safe: the task holds an `Arc<AccountSlot>` so the slot
                    // cannot be freed under it, and `process_order_cmd` rechecks
                    // the tombstone under the engine lock so a destroyed account
                    // refuses rather than resurrects. Other sessions may be bound
                    // to the same account and MUST see the mutation - a
                    // `SessionLease` is a counter on `AccountSlot.sessions`, never
                    // ownership of the account. The lifetime is bounded by the two
                    // budgets above, not by the connection.
                    tokio::spawn(delayed_act(DelayedAct {
                        cmd: order_cmd,
                        class: class.expect("a nonzero act delay implies an order-entry class"),
                        act_ms,
                        state: state.clone(),
                        slot: Arc::clone(lease.slot()),
                        lanes: lanes.clone(),
                        _ticket: ticket,
                        _permit: permit,
                    }));
                    continue;
                }
                // The read loop NEVER sleeps an act delay itself: `Paid` is
                // passed even when `act_ms` is zero, so a re-arm landing between
                // the load above and `process_order_cmd`'s own load cannot turn
                // this arm into an inline hold.
                match process_order_cmd(order_cmd, &state, lease.slot(), &lanes, ActDelay::Paid)
                    .await
                {
                    OrderOutcome::Produced {
                        events,
                        reservation,
                    }
                    | OrderOutcome::Refused {
                        events,
                        reservation,
                    } => {
                        debug_assert!(
                            events.iter().all(is_execution_event),
                            "order-entry events are always execution-category, never market data"
                        );
                        // One arrival instant for the whole batch: the engine
                        // produced these events together in one `process` call, so
                        // under an armed `DelayAcks` they share one deadline and
                        // land together ~ms late (see `spawn_exec_pump` for the
                        // per-event-deadline contract this anchors). The send
                        // cannot block - the held lane is unbounded by channel
                        // capacity, bounded by the byte budget already reserved
                        // above - so this arm never stalls the reader.
                        if lanes
                            .submit_produced(reservation, Instant::now(), class, events)
                            .is_err()
                        {
                            break None; // writer gone; the connection is tearing down
                        }
                    }
                    OrderOutcome::NotAdmitted(frame)
                    | OrderOutcome::Diagnostic(frame)
                    | OrderOutcome::Gone(frame) => {
                        if let Err(close) = send_admission(&lanes, frame) {
                            break Some(close);
                        }
                    }
                }
            }
        }
    };

    // The overload close jumps the held queue by the writer's biased select, so
    // it does not need a lane slot: the read loop has already stopped taking new
    // work and the writer stops on it.
    let closing = overload.is_some();
    if let Some(close) = overload {
        tracing::warn!(reason = %close.reason, "closing connection: admission overload");
        drop(lanes.send_close(close));
    } else {
        // The ordinary disconnect ALSO has to stop the writer explicitly, and
        // it did not have to before pending acts existed. The writer's loop ends
        // when both of its receivers close, i.e. when the last `Outbound` sender
        // is dropped - and a command detached by an armed act latency holds an
        // `ExecLanes` clone (hence a priority-lane sender) for as long as the
        // venue takes to act, which is up to an hour. Without this the teardown
        // below would park in `writer.await` for the whole remaining act window,
        // holding the session lease open and making teardown WAIT on a detached
        // task that is documented not to be waited on. A close on the priority
        // lane breaks the writer at once; the peer is already gone, so the frame
        // itself is best-effort and its send error is ignored.
        drop(lanes.send_close(CloseSpec {
            code: axum::extract::ws::close_code::NORMAL,
            reason: "session closed".into(),
        }));
    }

    // Cancel every replay first so the threads stop generating, then join them
    // (the writer task is still draining `rx`, so a thread blocked in a send
    // unblocks and observes the cancel promptly). Reaping the handles here means
    // a disconnect leaves no detached fanout task parked in `recv`/send
    // (E.7). Only after the threads are joined do we drop the last `tx` and let
    // the writer task finish.
    for subscription in subscriptions.values() {
        subscription.cancel.store(true, Ordering::Relaxed);
        subscription.cancel_wake.notify_waiters();
    }
    for (_, subscription) in subscriptions.drain() {
        quiesce_subscription(subscription).await;
    }
    if let Some(handle) = heartbeat {
        handle.abort();
        if let Err(e) = handle.await
            && !e.is_cancelled()
        {
            tracing::warn!(%e, "heartbeat task did not shut down cleanly");
        }
    }
    // Abort rather than let the pump drain gracefully: the client is already
    // gone, so an execution event still sleeping out an armed `DelayAcks`
    // window has nowhere to go. This is what keeps a disconnect from lingering
    // for the rest of that sleep - the pump held the only remaining sleep in
    // this connection, so once it is torn down `writer.await` below returns
    // promptly instead of waiting out up to an hour of armed delay.
    exec_pump.abort();
    if let Err(e) = exec_pump.await
        && !e.is_cancelled()
    {
        tracing::warn!(%e, "exec delay pump did not shut down cleanly");
    }
    drop(tx);
    drop(lanes);
    // Awaiting the writer unconditionally is not a terminal path when the
    // connection is being closed for overload: the writer may already be parked
    // in `sink.send()` against a peer that stopped reading, in which case the
    // close frame is never written and teardown blocks behind it forever - the
    // same hang, one layer down. A reasoned close is best-effort by nature;
    // releasing the connection's resources is not, so on timeout the writer is
    // aborted and the socket dropped. The ordinary-disconnect path keeps the
    // plain await: there is no peer to wait on, so it returns at once.
    let mut writer = writer;
    let joined = if closing {
        tokio::time::timeout(CLOSE_GRACE, &mut writer).await
    } else {
        Ok((&mut writer).await)
    };
    match joined {
        Ok(Err(e)) => tracing::warn!(%e, "writer task did not shut down cleanly"),
        Err(_) => {
            writer.abort();
            tracing::warn!(
                grace_ms = CLOSE_GRACE.as_millis(),
                "peer did not accept the overload close within the grace window; \
                 dropping the socket"
            );
        }
        Ok(Ok(())) => {}
    }
}

/// One order command the venue is taking `act_ms` to act on, detached from the
/// read loop so later commands can reach the engine first - which is the whole
/// point of an act latency and cannot happen while the command occupies the
/// strictly serial read loop. Everything after the sleep is exactly what the
/// inline path does, so the two cannot drift in WHAT they produce, only in when.
struct DelayedAct {
    cmd: ClientMessage,
    class: CommandClass,
    act_ms: u64,
    state: AppState,
    slot: Arc<AccountSlot>,
    lanes: ExecLanes,
    /// Per-connection pending-act ticket, released when the task ends wherever
    /// it ends.
    _ticket: Ticket,
    /// Process-wide pending-act permit, likewise.
    _permit: tokio::sync::OwnedSemaphorePermit,
}

/// Sleep the venue's act window off the read loop, then run the ordinary order
/// path and dispatch its outcome.
///
/// The sleep is read ONCE, at spawn: a `ClearDivergences` posted mid-sleep zeroes
/// the atomic but does not lift an act already being served, because the venue
/// has begun acting on this command. That is the deliberate opposite of the ACK
/// half, which the pump reads per event at dequeue and which therefore IS
/// liftable.
///
/// Two differences from the inline dispatch, both forced by being off the loop:
/// there is no read loop to break, so a `LaneClosed` simply returns (the
/// connection is already tearing down), and an overload `CloseSpec` is handed to
/// `send_close` directly, because no task can break another task's loop.
async fn delayed_act(act: DelayedAct) {
    tokio::time::sleep(
        act.state
            .sim
            .wall_duration(sim_duration_from_millis(act.act_ms)),
    )
    .await;
    match process_order_cmd(act.cmd, &act.state, &act.slot, &act.lanes, ActDelay::Paid).await {
        OrderOutcome::Produced {
            events,
            reservation,
        }
        | OrderOutcome::Refused {
            events,
            reservation,
        } => {
            debug_assert!(
                events.iter().all(is_execution_event),
                "order-entry events are always execution-category, never market data"
            );
            // `Err` is `LaneClosed`: the client is gone, the mutation already
            // landed, and only the acknowledgment has nowhere to go.
            drop(
                act.lanes
                    .submit_produced(reservation, Instant::now(), Some(act.class), events),
            );
        }
        OrderOutcome::NotAdmitted(frame)
        | OrderOutcome::Diagnostic(frame)
        | OrderOutcome::Gone(frame) => {
            if let Err(close) = send_admission(&act.lanes, frame) {
                drop(act.lanes.send_close(close));
            }
        }
    }
}

/// Delay queued execution events without head-of-line-blocking the market-data
/// feed behind them, honoring the `DelayAcks` contract per event.
///
/// The hop exists because the writer used to `sleep` inline on an execution
/// event before its next `rx.recv()`, which stalled every tick already queued
/// behind it for the entire armed `DelayAcks` window (up to one hour) - and,
/// on disconnect, kept the writer (and the connection's teardown) parked for
/// whatever remained of that sleep even though the socket was already gone.
/// This pump reads execution-category events off their own channel and forwards
/// into `out`, so a long delay only holds up further exec traffic. The senders
/// are the order-entry handling in `handle_socket` (engine events) and
/// `send_exec_protocol_error` (ProtocolError diagnostics, which classify as
/// execution and so honor DelayAcks like any other exec event - S10).
///
/// The delay deadline is PER EVENT, anchored at the instant the event was
/// enqueued: `arrival + armed delay`. Sleeping the full window between
/// consecutive dequeues would compound instead - a single submit produces
/// OrderAccepted + OrderFilled + AccountState, and under `DelayAcks ms` a
/// serial per-dequeue sleep delivered them at +ms, +2ms, +3ms while the
/// contract holds EVERY outbound execution event by ms. Anchoring each
/// deadline at arrival delivers all three ~ms late: an event whose deadline
/// already elapsed while a predecessor slept has no remaining wait and
/// forwards immediately. Arrival order is preserved (one `while` loop over
/// the channel) and arrival instants are monotone, so same-delay deadlines
/// are monotone too and the wire order matches the engine's event order
/// exactly as it did in the old inline path. The armed value is read per
/// event at dequeue, so re-arming or `ClearDivergences` applies to everything
/// still queued; the window rides the sim axis via `wall_duration`, matching
/// every other ms-window control.
pub(crate) fn spawn_exec_pump(
    mut exec_rx: mpsc::UnboundedReceiver<HeldFrame>,
    slot: Arc<AccountSlot>,
    sim: SimClock,
    out: mpsc::Sender<Outbound>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(HeldFrame {
            arrived,
            class,
            frame,
        }) = exec_rx.recv().await
        {
            // The two windows COMPOSE: a per-command ack latency ADDS to any
            // armed `DelayAcks` rather than replacing it, matching the rule
            // `BASELINE_LATENCY` states for the adapter's inbound latency. Both
            // operands are bounded by `MAX_DIVERGENCE_MS`, so the sum cannot
            // overflow; the saturating add is belt and braces.
            let delay = slot
                .delay_ms
                .load(Ordering::Relaxed)
                .saturating_add(class.map_or(0, |class| slot.ack_ms(class)));
            if delay > 0 {
                // `deadline - now` phrased as a saturating remainder so a
                // deadline already in the past sleeps zero, and a pathological
                // `arrival + delay` can never overflow `Instant` arithmetic.
                let hold = sim.wall_duration(sim_duration_from_millis(delay));
                let remaining = hold.saturating_sub(arrived.elapsed());
                if !remaining.is_zero() {
                    tokio::time::sleep(remaining).await;
                }
            }
            if out.send(Outbound::Frame(frame)).await.is_err() {
                break; // writer gone
            }
        }
    })
}

/// Explicit wall-clock floor on the heartbeat period (S21). `wall_duration`
/// already clamps a scaled span to 1ns so `interval_at` never sees a zero
/// Duration, but under a large `speed` the configured sim-ms cadence still
/// collapses toward the timer granularity and the heartbeat becomes a ~kHz
/// per-socket flood. Pinning an explicit 1ms floor bounds the rate without
/// depending on tokio's internal coalescing; a heartbeat finer than a
/// millisecond carries no liveness signal a coarser one does not.
pub(crate) const MIN_HEARTBEAT_WALL: Duration = Duration::from_millis(1);

/// The wall period a heartbeat ticks on: the configured sim-ms cadence mapped
/// onto the wall axis, floored at [`MIN_HEARTBEAT_WALL`].
pub(crate) fn heartbeat_period(interval_ms: u64, sim: SimClock) -> Duration {
    sim.wall_duration(sim_duration_from_millis(interval_ms))
        .max(MIN_HEARTBEAT_WALL)
}

/// Feed per-session server liveness frames into the same channel the writer
/// gates and serializes, keeping socket writes single-owned and ordered.
fn spawn_heartbeat(
    interval_ms: u64,
    sim: SimClock,
    tx: mpsc::Sender<Outbound>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let period = heartbeat_period(interval_ms, sim);
        // `tokio::time::interval` fires its first tick immediately rather than
        // after one period, so a plain `interval(period)` would emit a
        // `Heartbeat` at t=0 the instant the socket opens - before the client
        // has even sent its first `Subscribe`, and one full interval earlier
        // than every tick after it. `interval_at` with an explicit first
        // deadline one period out keeps the cadence uniform from the start.
        let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        // Skip missed ticks rather than the default Burst: if the runtime stalls,
        // a heartbeat should resume its cadence from now, not fire a catch-up
        // burst of every deadline it slept through (S21).
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            // Serialized here like every other producer, and charged to no
            // budget: the heartbeat is server-generated at a bounded cadence,
            // it is not engine output, and it predates the lanes. The `expect`
            // is a `{ts_event: u64}` - unserializable by construction.
            if tx
                .send(Outbound::Frame(OutboundFrame {
                    payload: Arc::from(
                        serde_json::to_string(&ServerMessage::Heartbeat {
                            ts_event: sim_now_ns(sim),
                        })
                        .expect("Heartbeat serializes"),
                    ),
                    is_market_data: false,
                    charge: None,
                    slot: None,
                }))
                .await
                .is_err()
            {
                break;
            }
        }
    })
}

/// One subscription's live feed: a tokio task fanning one tape's frames into
/// this connection's outbound channel, after replaying whatever backfill the
/// subscriber's own seek target demands.
pub(crate) struct Subscription {
    /// Retained for the cheap synchronous checks the loop bodies already do,
    /// but it CANNOT wake a parked task on its own.
    pub(crate) cancel: Arc<AtomicBool>,
    /// The wakeup half. Raising `cancel` is always followed by
    /// `cancel_wake.notify_waiters()`, and every await in the task is a
    /// `select!` against it. Without this, a fanout task parked in `recv` on an
    /// idle tape (mean cadence ~7 s in identity mode) or in `reserve` on a full
    /// connection channel would delay unsubscribe, resubscribe and disconnect
    /// by seconds or indefinitely - and disconnect teardown awaits every task,
    /// so the delay would compound across subscriptions.
    pub(crate) cancel_wake: Arc<Notify>,
    pub(crate) task: tokio::task::JoinHandle<()>,
    /// The last `ts_event` this subscription successfully sent (or
    /// [`NO_TICK_SENT`]), so a resubscribe of this symbol can seek its
    /// replacement strictly past whatever this stream already delivered.
    pub(crate) last_sent_ts: Arc<AtomicU64>,
}

/// Sentinel `last_sent_ts` for a subscription that has not yet sent a tick. A
/// real `ts_event` this large is not reachable within the tape's lifetime, so
/// it is distinguishable from any genuine timestamp without an `Option`.
pub(crate) const NO_TICK_SENT: u64 = u64::MAX;

/// Cancel a subscription's fanout task and await its exit.
///
/// No `abort`: the task must run to its own exit so the `last_sent_ts` it may
/// be mid-storing is final. Cancel latency is bounded because EVERY await point
/// in the task - the `recv`, the `tx.reserve()`, the backfill pacing sleep and
/// the startup handshake - is a `select!` whose other arm is
/// `cancel_wake.notified()`. "Woken by the next frame" is explicitly NOT the
/// bound: an idle tape can be seconds away from its next frame. Returning only
/// once the task has ended is what guarantees quiescence: callers rely on it so
/// a replaced stream cannot interleave a stale tick after its successor begins
/// (E.6), and so disconnect reaps every task.
pub(crate) async fn quiesce_subscription(subscription: Subscription) {
    subscription.cancel.store(true, Ordering::Relaxed);
    subscription.cancel_wake.notify_waiters();
    if let Err(e) = subscription.task.await
        && !e.is_cancelled()
    {
        tracing::error!(%e, "fanout task panicked; its market-data feed ended silently");
    }
}

/// Quiesce a symbol's in-flight subscription and return its resume floor: the
/// last `ts_event` it successfully sent, or `None` if it never sent one.
///
/// The floor is loaded only AFTER the task has ended, and that ordering is
/// load-bearing. The task's cancel checks bracket the send, but a send already
/// past its check completes and stores `last_sent_ts` even if `cancel` lands
/// mid-flight - so between a pre-join load and the task's exit, one more tick
/// (T2) can enter the shared channel and advance `last_sent_ts` past the loaded
/// value (T1). A floor of T1 makes the replacement seek `T1 + 1` and regenerate
/// everything in `(T1, T2]`: duplicate frames on the wire, breaking the
/// ascending-`ts_event` ordering the adapter's cursor relies on - exactly the
/// E.5/E.6 seam the resume floor exists to close.
pub(crate) async fn quiesce_and_resume_floor(old: Subscription) -> Option<u64> {
    let last_sent_ts = Arc::clone(&old.last_sent_ts);
    quiesce_subscription(old).await;
    let last_sent = last_sent_ts.load(Ordering::Relaxed);
    (last_sent != NO_TICK_SENT).then_some(last_sent)
}

pub(crate) struct FanoutSpawn {
    pub(crate) symbol: String,
    pub(crate) generation: u64,
    pub(crate) lease: TapeLease,
    /// The seek target, from `resume_seek_target(start_ts, resume_floor)`.
    /// `None` means "start at the tape's attach point".
    pub(crate) target: Option<u64>,
    pub(crate) regime: Option<MarketRegime>,
    pub(crate) profiles: Arc<source::InstrumentProfiles>,
    pub(crate) data_origin: u64,
    pub(crate) tx: mpsc::Sender<Outbound>,
    /// The session's outbound lanes. This task's diagnostics are admission
    /// truth, so they ride the PRIORITY lane - ahead of held traffic and exempt
    /// from `DelayAcks` - instead of the market-data `tx`.
    pub(crate) lanes: ExecLanes,
    /// The promise reserved for this subscription's ONE possible diagnostic,
    /// taken by the handler BEFORE it quiesced whatever stream this one
    /// replaces. A fanout task spends it on `SeekBudgetExhausted` (before any
    /// frame is delivered) or on `FeedLagged`, never both: the former always
    /// ends the task, and the latter is only reachable once the feed is
    /// running. That exclusivity is what makes one ticket sufficient, and it is
    /// exactly the invariant a future edit adding a third mid-stream diagnostic
    /// would break.
    pub(crate) diag_ticket: Option<Ticket>,
    pub(crate) cancel: Arc<AtomicBool>,
    pub(crate) cancel_wake: Arc<Notify>,
    pub(crate) last_sent_ts: Arc<AtomicU64>,
    pub(crate) sim: SimClock,
    pub(crate) speed: f64,
    pub(crate) gap_cap_ms: u64,
}

/// Register on `notify` BEFORE reading the state it guards.
///
/// `Notify::notified()` only enqueues a waiter when the future is first polled,
/// and `notify_waiters()` leaves no permit behind, so a notification that fires
/// between a state check and the `select!` that awaits it is LOST - the task
/// then parks until the next unrelated wakeup, which on a poisoned or idle tape
/// is never, and the quiesce/disconnect awaiting that task hangs with it.
/// `enable()` enqueues the waiter up front, so any notification after this
/// point wakes the returned future no matter when it is first polled.
fn armed(notify: &Notify) -> std::pin::Pin<Box<tokio::sync::futures::Notified<'_>>> {
    let mut fut = Box::pin(notify.notified());
    fut.as_mut().enable();
    fut
}

/// One subscription's feed: resolve the tape's startup state, replay whatever
/// backfill this subscriber's own seek target demands, then forward the tape's
/// broadcast frames.
pub(crate) fn spawn_fanout(spawn: FanoutSpawn) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let FanoutSpawn {
            symbol,
            generation,
            mut lease,
            target,
            regime,
            profiles,
            data_origin,
            tx,
            lanes,
            mut diag_ticket,
            cancel,
            cancel_wake,
            last_sent_ts,
            sim,
            speed,
            gap_cap_ms,
        } = spawn;
        let dead_seek = |diag_ticket: &mut Option<Ticket>| {
            spend_diagnostic(
                &lanes,
                diag_ticket,
                generation,
                &symbol,
                SubscriptionIssue::SeekBudgetExhausted,
                sim.sim_ns(now_ns()),
            );
        };

        // Phase 0: fix the two values the rest of the task turns on.
        //
        // - `backfill_bound`: the last `ts_event` phase 1 may emit, i.e. the
        //   frame just before the first one this lease's receiver will hold.
        // - `attach_floor`: frames at or below this were already delivered by
        //   whoever this subscription replaced, and are filtered out of the
        //   live phase.
        let starting = matches!(lease.attach, CursorState::Starting);
        let (backfill_bound, attach_floor) = match lease.attach {
            // Attached mid-stream: the receiver starts after `ts` (modulo the
            // store-before-send duplicate window), so backfill covers
            // `(target ..= ts]` and everything at or below `ts` is filtered.
            CursorState::Live(ts) => (Some(ts), ts),
            CursorState::Poisoned => {
                dead_seek(&mut diag_ticket);
                return;
            }
            // Attached before the tape committed anything: this receiver holds
            // the tape's FIRST frame, whatever it turns out to be, and that
            // frame is the first tick at or after the tape's seek target. So
            // the boundary is knowable WITHOUT waiting for the tape to produce
            // anything, which is what keeps an explicit historical `start_ts`
            // honest on a brand-new tape: waiting would stall a windowed
            // subscribe behind the live cadence (up to `gap_cap_ms`), silently
            // turning it into a live one. The floor stays 0 because nothing has
            // been delivered to this subscriber by anyone.
            CursorState::Starting => (Some(lease.seek_target_ns().saturating_sub(1)), 0),
        };
        // Everything at or below this has already been delivered by this
        // subscription or by the predecessor whose floor produced `target`.
        // The `target - 1` term is not implied by the others: when
        // `target > backfill_bound` phase 1 is skipped, and filtering on the
        // attach floor alone would re-deliver the frames in
        // `(attach_floor, target)` that a predecessor on a DIFFERENT tape (any
        // regime change on resubscribe) already sent - a duplicated or
        // regressed `ts_event` across exactly the E.5/E.6 resubscribe seam.
        let mut high_water = target.map_or(0, |t| t.saturating_sub(1)).max(attach_floor);

        // Phase 1: backfill `(target ..= attach_ts]` from a PRIVATE history
        // source. The clean case rides the process-global checkpoint index, so
        // POSITIONING is O(K) rather than O(span); the number of ticks EMITTED
        // is bounded only by how far behind the cursor the target sits, which is
        // why this is paced and streamed rather than materialized.
        if let (Some(target), Some(cursor)) = (target, backfill_bound)
            && target <= cursor
        {
            // The structurally doomed configuration, refused up front rather
            // than discovered minutes in. Identity gap pacing with the cap
            // disabled advances the backfill at exactly the sim-rate the live
            // tape advances at, so the gap never closes and the bounded ring
            // ends the feed with `FeedLagged` once it has turned over. Only a
            // span the ring cannot outlive is refused: a short backfill
            // finishes before the turnover and is perfectly serviceable.
            if sim.is_identity()
                && speed > 0.0
                && gap_cap_ms == 0
                && lease
                    .tape
                    .frames_behind(target)
                    .is_some_and(|frames| frames > lease.ring_depth() as u64)
            {
                tracing::warn!(
                    %symbol,
                    target,
                    cursor,
                    "backfill cannot catch up under uncapped identity pacing; refusing"
                );
                spend_diagnostic(
                    &lanes,
                    &mut diag_ticket,
                    generation,
                    &symbol,
                    SubscriptionIssue::FeedLagged { skipped: 0 },
                    sim.sim_ns(now_ns()),
                );
                return;
            }
            // Synthesis runs on a blocking thread, feeding a BOUNDED channel:
            // `MergeSource` is not `Send`, so it could not be held across an
            // await anyway, and a tokio worker is the wrong place for a walk
            // that can be millions of ticks long. The bound is what keeps a
            // long backfill from materializing: the producer parks on a full
            // channel instead of buffering the whole span in memory, and it
            // exits the moment this task drops the receiver.
            let (backfill_tx, mut backfill_rx) = mpsc::channel::<(u64, Arc<str>)>(64);
            let backfill = {
                let symbol = symbol.clone();
                let profiles = Arc::clone(&profiles);
                tokio::task::spawn_blocking(move || {
                    let mut history = source::build_history_source(
                        &symbol,
                        Some(target),
                        regime,
                        &profiles,
                        data_origin,
                    )?;
                    // The generated tape never runs dry on its own, so a `None`
                    // FIRST tick means the positioning seek exhausted its budget.
                    let mut tick = history.next_tick()?;
                    loop {
                        if tick.ts_event() > cursor {
                            return Some(());
                        }
                        let ts = tick.ts_event();
                        let msg = match tick {
                            TickEvent::Trade(t) => ServerMessage::Trade(t),
                            TickEvent::Quote(q) => ServerMessage::Quote(q),
                        };
                        let payload = match serde_json::to_string(&msg) {
                            Ok(payload) => payload,
                            Err(e) => {
                                tracing::error!(%e, "could not serialize backfill frame");
                                return Some(());
                            }
                        };
                        if backfill_tx.blocking_send((ts, Arc::from(payload))).is_err() {
                            return Some(());
                        }
                        match history.next_tick() {
                            Some(next) => tick = next,
                            None => return Some(()),
                        }
                    }
                })
            };
            // Same pacing-anchor discipline as the tape thread: one wall read
            // paired with one monotonic read, so nothing here re-consults
            // `CLOCK_REALTIME` and an NTP step can neither stall nor burst the
            // backfill.
            let wall_anchor = now_ns();
            let instant_anchor = tokio::time::Instant::now();
            let mut prev_ts: Option<u64> = None;
            let mut next_deadline: Option<u64> = None;
            let mut backfilled = 0usize;
            loop {
                let cancelled = armed(&cancel_wake);
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let Some((ts, payload)) = (tokio::select! {
                    () = cancelled => return,
                    frame = backfill_rx.recv() => frame,
                }) else {
                    break;
                };
                backfilled += 1;
                // Backfill pacing rules, unchanged from the private replay's
                // explicit-`start_ts` path: accelerated mode deadline-paces
                // against `sim.wall_ns`, so already-elapsed deadlines emit at
                // full speed and catch-up terminates; identity mode gap-paces
                // capped at `gap_cap_ms`, which at the default 1000 ms against
                // the tape's multi-second mean cadence catches up several times
                // faster than the clock.
                let deadline = if !sim.is_identity() {
                    Some(sim.wall_ns(ts))
                } else if speed > 0.0 {
                    let wait = prev_ts.map(|prev| {
                        let mut wait_ns = (ts.saturating_sub(prev) as f64 / speed) as u64;
                        if gap_cap_ms > 0 {
                            wait_ns = wait_ns.min(gap_cap_ms.saturating_mul(1_000_000));
                        }
                        wait_ns
                    });
                    prev_ts = Some(ts);
                    wait.map(|wait_ns| {
                        let d = next_deadline.unwrap_or(wall_anchor).saturating_add(wait_ns);
                        next_deadline = Some(d);
                        d
                    })
                } else {
                    None
                };
                if let Some(deadline) = deadline
                    && sleep_until_wall_cancellable_async(
                        deadline,
                        wall_anchor,
                        instant_anchor,
                        &cancel,
                        &cancel_wake,
                    )
                    .await
                    .is_err()
                {
                    return;
                }
                if send_cancellable_async(
                    &tx,
                    Outbound::Frame(OutboundFrame {
                        payload,
                        is_market_data: true,
                        charge: None,
                        slot: None,
                    }),
                    &cancel,
                    &cancel_wake,
                )
                .await
                .is_err()
                {
                    return;
                }
                last_sent_ts.store(ts, Ordering::Relaxed);
                high_water = high_water.max(ts);
            }
            // No source at all, or a first tick that failed the bounded seek:
            // identical to what a private replay reported today.
            if backfilled == 0 && !matches!(backfill.await, Ok(Some(()))) {
                dead_seek(&mut diag_ticket);
                return;
            }
        }

        // The seek verdict, resolved after phase 1 rather than before it: a
        // `Starting` lease attached while the tape's seek was possibly still in
        // flight, and this is the ONLY place `SeekBudgetExhausted` is emitted,
        // so a subscriber attaching during a seek in flight and one attaching
        // after it failed get the identical answer.
        if starting {
            loop {
                let woken = armed(lease.wake());
                let cancelled = armed(&cancel_wake);
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                match lease.tape.cursor_state() {
                    CursorState::Poisoned => {
                        if last_sent_ts.load(Ordering::Relaxed) == NO_TICK_SENT {
                            dead_seek(&mut diag_ticket);
                        } else {
                            // A backfill this subscriber already received proves
                            // the walk is servable, so this is not the dead-seek
                            // signal; the live half is simply over.
                            tracing::warn!(%symbol, "tape poisoned after a backfill; feed ended");
                        }
                        return;
                    }
                    CursorState::Live(_) => break,
                    CursorState::Starting => {
                        tokio::select! {
                            () = cancelled => return,
                            () = woken => {}
                        }
                    }
                }
            }
        }

        // Phase 2: the live feed. The two phases are the same deterministic
        // walk, so the seam is exact - the private backfill reproduced the
        // tape's own bytes for that interval, and the broadcast ring has been
        // accumulating live frames throughout.
        loop {
            let cancelled = armed(&cancel_wake);
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            let frame = tokio::select! {
                () = cancelled => return,
                frame = lease.rx.recv() => frame,
            };
            match frame {
                // The store-before-send duplicate window, the pre-`target`
                // window, and anything the backfill already covered.
                Ok(frame) if frame.ts_event <= high_water => {
                    lease.progress.store(frame.seq, Ordering::Relaxed);
                }
                Ok(frame) => {
                    let seq = frame.seq;
                    let ts_event = frame.ts_event;
                    if send_cancellable_async(
                        &tx,
                        Outbound::Frame(OutboundFrame {
                            payload: frame.payload,
                            is_market_data: true,
                            charge: None,
                            slot: None,
                        }),
                        &cancel,
                        &cancel_wake,
                    )
                    .await
                    .is_err()
                    {
                        return;
                    }
                    high_water = ts_event;
                    last_sent_ts.store(ts_event, Ordering::Relaxed);
                    // The zero-speed headroom throttle's input.
                    lease.progress.store(seq, Ordering::Relaxed);
                }
                // A VENUE FAULT, not a divergence and not a refusal the client
                // could have planned for. A shared tape cannot be stalled by one
                // slow subscriber the way a private replay could, so a ring that
                // turns over means the venue has lost market data it already
                // promised to deliver in ascending order - an unarmed hole in a
                // stream whose whole contract is that perturbations are
                // deliberate. On loopback at the default ring depth this is
                // unreachable short of a client stalling for hours, so if it
                // fires something is badly wrong rather than merely slow.
                //
                // The connection therefore DIES instead of continuing past the
                // gap: a forward-validation run that keeps streaming with
                // silently-missing ticks yields a result that looks clean and is
                // not. The named diagnostic still rides the priority lane so the
                // cause is on the wire, and the close states it again in case
                // the socket dies first.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::error!(
                        %symbol,
                        skipped,
                        "VENUE FAULT: tape ring turned over and the venue lost \
                         market data it had promised; killing the connection \
                         rather than serving a hole"
                    );
                    spend_diagnostic(
                        &lanes,
                        &mut diag_ticket,
                        generation,
                        &symbol,
                        SubscriptionIssue::FeedLagged { skipped },
                        sim.sim_ns(now_ns()),
                    );
                    // A send failure here means the writer is already gone, so
                    // the connection is dying anyway - which is the outcome.
                    drop(
                        tx.send(Outbound::Close(CloseSpec::venue_fault(format!(
                            "venue fault: lost {skipped} ticks of {symbol}; the tape ring \
                             turned over and this feed has an unarmed gap"
                        ))))
                        .await,
                    );
                    return;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    if last_sent_ts.load(Ordering::Relaxed) == NO_TICK_SENT {
                        // The dead seek, if nothing else has reported it.
                        dead_seek(&mut diag_ticket);
                    } else {
                        tracing::debug!(%symbol, "tape ended");
                    }
                    return;
                }
            }
        }
    })
}

/// The async twin of the private replay's blocking send: `tx.reserve()` raced
/// against cancellation, so a full connection channel applies backpressure to
/// THIS subscriber only - never to the tape, which every other subscriber on
/// the symbol shares - and a cancel while parked returns promptly.
async fn send_cancellable_async<T>(
    tx: &mpsc::Sender<T>,
    msg: T,
    cancel: &AtomicBool,
    wake: &Notify,
) -> Result<(), ()> {
    let cancelled = armed(wake);
    if cancel.load(Ordering::Relaxed) {
        return Err(());
    }
    tokio::select! {
        () = cancelled => Err(()),
        permit = tx.reserve() => match permit {
            Ok(permit) if !cancel.load(Ordering::Relaxed) => {
                permit.send(msg);
                Ok(())
            }
            _ => Err(()),
        },
    }
}

/// Sleep until `target_wall_ns`, mapped through the caller's NTP-immune
/// `(wall_anchor, instant_anchor)` pairing, in slices bounded by
/// [`crate::tape::TAPE_SLEEP_POLL`] and interruptible by cancellation.
/// `Err(())` means the subscription was cancelled mid-gap, so a quiesce during
/// a long backfill is bounded at one slice rather than at the pacing gap.
async fn sleep_until_wall_cancellable_async(
    target_wall_ns: u64,
    wall_anchor: u64,
    instant_anchor: tokio::time::Instant,
    cancel: &AtomicBool,
    wake: &Notify,
) -> Result<(), ()> {
    let due = instant_anchor + Duration::from_nanos(target_wall_ns.saturating_sub(wall_anchor));
    loop {
        let cancelled = armed(wake);
        if cancel.load(Ordering::Relaxed) {
            return Err(());
        }
        let now = tokio::time::Instant::now();
        if now >= due {
            return Ok(());
        }
        let slice = (due - now).min(crate::tape::TAPE_SLEEP_POLL);
        tokio::select! {
            () = cancelled => return Err(()),
            () = tokio::time::sleep(slice) => {}
        }
    }
}

/// Sort + dedup the requested symbols so a single subscription naming a symbol
/// twice does not spawn (then immediately quiesce) two streams for it.
pub(crate) fn dedup_symbols(mut symbols: Vec<String>) -> Vec<String> {
    symbols.sort();
    symbols.dedup();
    symbols
}

/// Whether a frame is order-lifecycle/account traffic, as opposed to market
/// data OR admission truth.
///
/// Delegates to the protocol's shared classifier (`ServerMessage::category`) so
/// the server's `DelayAcks` delay path and the adapter's inbound latency
/// bucketing decide this from one source of truth - notably `AccountState`, an
/// account event that reports balances and positions moved by fills, which both
/// ends agree rides the execution path. Note the three-way split: an admission
/// frame (`AdmissionRejected`, `ProtocolError`) is NEITHER execution nor market
/// data, because it is not something the engine produced, which is exactly what
/// exempts it from the knob that holds engine output.
pub(crate) fn is_execution_event(msg: &ServerMessage) -> bool {
    msg.category().is_execution()
}

/// The seek target a subscription's fanout task backfills from.
///
/// A live continuation (`start_ts: None`) that replaces a just-quiesced stream
/// for this symbol must not re-seek to a freshly-sampled `sim_now` unguarded:
/// quiescing only stops the OLD thread from enqueuing anything further, it
/// does not purge what it already enqueued, so a fresh `sim_now` seek could
/// land at-or-before that already-delivered tick (a duplicate) or skip
/// everything between it and `sim_now` (a gap). `resume_floor` is that
/// predecessor's last successfully-sent `ts_event`; seeking one nanosecond
/// past it - unconditionally, not clamped to a freshly-sampled `sim_now` -
/// resumes the exact same walk exactly where the predecessor left off,
/// rather than fast-forwarding past whatever it hadn't gotten to generate yet
/// (which a `.max(sim_now)` would silently skip). An explicit `start_ts` is a
/// deliberate historical/resume request from the client and is honored as
/// given, never adjusted by a floor meant for the no-`start_ts`
/// live-continuation case.
pub(crate) fn resume_seek_target(start_ts: Option<u64>, resume_floor: Option<u64>) -> Option<u64> {
    match (start_ts, resume_floor) {
        (Some(ts), _) => Some(ts),
        (None, Some(floor)) => Some(floor.saturating_add(1)),
        (None, None) => None,
    }
}

/// Spend a replay's reserved diagnostic promise on one single-entry
/// `SubscriptionIssues`. The entry carries the generation the replay was
/// spawned with, not whatever is current by the time the fault is discovered:
/// this is the ASYNCHRONOUS diagnostic the whole per-entry generation exists to
/// make discardable by a client that has since resubscribed.
///
/// Returns `()`, never a `Result<(), CloseSpec>`: it runs on the replay OS
/// thread, which has no channel by which a close decision could reach the socket
/// owner, so every failure here is best-effort logging.
///
/// Converting a promise into a queued frame re-checks the QUEUE budget: a
/// promise guarantees the venue has ACCOUNTED for this diagnostic, not that the
/// priority queue can never be full. Conflating those two is what makes a
/// single-pool design wrong. A full queue or a closed lane means the client is
/// already gone or is not reading at all, and the thread exits either way -
/// this runs on its own OS thread, so it may neither block nor stall anything.
///
/// Takes the ticket by `&mut Option` so spending it CONSUMES it: a second call
/// finds `None`, logs loudly, and does not silently emit an unaccounted frame.
fn spend_diagnostic(
    lanes: &ExecLanes,
    promise: &mut Option<Ticket>,
    generation: u64,
    symbol: &str,
    issue: SubscriptionIssue,
    ts_event: u64,
) {
    let Some(promise) = promise.take() else {
        tracing::error!(
            %symbol,
            "replay diagnostic promise already spent; a second diagnostic is unaccounted for"
        );
        return;
    };
    // The promise is released here regardless: it has done its job, and the
    // queue slot is what bounds the frame from here on.
    drop(promise);
    let Some(slot) = lanes.reserve_admission() else {
        tracing::warn!(%symbol, "priority lane full; replay diagnostic undeliverable");
        return;
    };
    if lanes
        .emit_admission(
            slot,
            ServerMessage::SubscriptionIssues {
                entries: vec![SubscriptionOutcome {
                    generation,
                    symbol: symbol.to_owned(),
                    issue,
                }],
                issues_total: 1,
                refusals_total: 1,
                ts_event,
            },
        )
        .is_err()
    {
        tracing::debug!(%symbol, generation, "subscription issue undeliverable; client gone");
    }
}
