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
    ACCOUNT_QUERY_PARAM, AccountId, ClientMessage, MarketRegime, ServerMessage, SimClock,
    SubscriptionIssue, SubscriptionOutcome,
};
use tokio::sync::{OwnedSemaphorePermit, mpsc};

use crate::accounts::{AccountSlot, SessionLease};
use crate::admission::{
    CLOSE_GRACE, CloseSpec, ExecLanes, HeldFrame, Outbound, OutboundFrame, Ticket,
};
use crate::config::{build_admission_limits, now_ns, sim_duration_from_millis, sim_now_ns};
use crate::http::{
    AppState, OrderOutcome, process_order_cmd, strip_unfireable_reopen_gap,
    validate_regime_or_clean,
};
use crate::source;

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
            .send(Message::Text(frame.payload.into()))
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
    let lease = SessionLease::acquire(Arc::clone(&slot), crate::config::now_ns());
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
    // One replay stream PER SYMBOL, not per sorted symbol-set. Keying per set let
    // overlapping subscriptions (`[A,B]` then `[B,C]`) spawn two independent
    // replays both emitting `B` from independent generators/clocks, so the client
    // saw duplicated, interleaved, out-of-order-per-symbol `B` trades - breaking
    // the ascending-`ts_event` ordering the adapter's `PollCursor` relies on
    // (E.5). With a per-symbol map a given symbol is fed by exactly one stream:
    // re-subscribing a symbol already in flight quiesces (cancels + joins) the old
    // stream before the replacement emits, so no stale tick interleaves at the
    // seam (E.6); the handles are tracked and reaped so threads cannot pile up
    // under connect/subscribe/disconnect churn (E.7).
    let mut replays: HashMap<String, Replay> = HashMap::new();
    let mut generations: HashMap<String, u64> = HashMap::new();
    let sim = state.sim;

    // `tx`/`rx` carries everything the writer paces normally: market data
    // straight from the replay threads and the heartbeat, and execution output
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
            ClientMessage::Subscribe { subscriptions } => {
                // Cardinality and per-symbol length are capped at the boundary
                // (a malformed request, answered with the untargeted
                // diagnostic) so one read-loop iteration cannot be made to
                // demand 100k per-symbol reservations, and so a symbol that
                // reaches the wire is bounded by `MAX_SYMBOL_LEN` - which is
                // what makes the coalesced refusal frame's ceiling provable.
                if let Err(reason) = mogwai_protocol::validate_subscriptions(&subscriptions) {
                    tracing::warn!(reason, count = subscriptions.len(), "refusing subscribe");
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
                for entry in subscriptions {
                    let symbol = entry.symbol;
                    // Unknown symbols get their diagnostic here rather than from a
                    // spawned replay thread: whether the venue lists a symbol is a
                    // cheap synchronous check, so there is no reason to spin up an
                    // OS thread that immediately exits and leaves a dead entry in
                    // `replays` (S22). The dead-SEEK diagnostic - knowable only by
                    // actually running the positioning seek - still comes from the
                    // thread in `spawn_replay`.
                    //
                    // Note this arm `continue`s BEFORE the quiesce and before
                    // the permit acquire, so a refused symbol never consumes a
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
                    // high-water map, not against `replays`: an Unsubscribe, or
                    // a quiesce followed by a capacity refusal, removes the
                    // replay, and enforcing off `replays` would then accept a
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
                    let resume_floor = if let Some(old) = replays.remove(&symbol) {
                        quiesce_and_resume_floor(old).await
                    } else {
                        None
                    };
                    // Ration the global replay-thread pool (S22a): every symbol
                    // stream is a dedicated OS thread, so without a ceiling a
                    // fleet of connections each subscribing the whole catalog
                    // exhausts the process thread limit. Acquire AFTER the
                    // quiesce above so a resubscribe of THIS symbol - which just
                    // released the predecessor's permit as it joined - reclaims
                    // it here rather than deadlocking against its own cap-of-one.
                    // A cap of 0 sizes the pool at MAX_PERMITS, so this branch
                    // never trips when the cap is disabled. On exhaustion, refuse
                    // this symbol with a ProtocolError (the same wire signal an
                    // unservable subscribe uses) and leave the running streams
                    // untouched.
                    let permit = match Arc::clone(&state.replay_permits).try_acquire_owned() {
                        Ok(permit) => permit,
                        Err(_) => {
                            tracing::warn!(
                                %symbol,
                                cap = state.cfg.max_concurrent_replays,
                                "replay capacity reached; subscribe refused"
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
                    let last_sent_ts = Arc::new(AtomicU64::new(NO_TICK_SENT));
                    let handle = spawn_replay(ReplaySpawn {
                        symbol: symbol.clone(),
                        generation: entry.generation,
                        start_ts,
                        regime,
                        speed: state.cfg.speed,
                        gap_cap_ms: state.cfg.gap_cap_ms,
                        profiles: Arc::clone(&state.profiles),
                        sim: state.sim,
                        data_origin: state.data_origin_ns,
                        tx: tx.clone(),
                        lanes: lanes.clone(),
                        diag_ticket: Some(diag_ticket),
                        cancel: Arc::clone(&cancel),
                        resume_floor,
                        last_sent_ts: Arc::clone(&last_sent_ts),
                        permit: Some(permit),
                    });
                    replays.insert(
                        symbol,
                        Replay {
                            cancel,
                            handle,
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
                    if let Some(old) = replays.remove(&symbol) {
                        quiesce_replay(old).await;
                    }
                }
            }
            order_cmd => match process_order_cmd(order_cmd, &state, lease.slot(), &lanes).await {
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
                        .submit_produced(reservation, Instant::now(), events)
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
            },
        }
    };

    // The overload close jumps the held queue by the writer's biased select, so
    // it does not need a lane slot: the read loop has already stopped taking new
    // work and the writer stops on it.
    let closing = overload.is_some();
    if let Some(close) = overload {
        tracing::warn!(reason = %close.reason, "closing connection: admission overload");
        drop(lanes.send_close(close));
    }

    // Cancel every replay first so the threads stop generating, then join them
    // (the writer task is still draining `rx`, so a thread blocked in a send
    // unblocks and observes the cancel promptly). Reaping the handles here means
    // a disconnect leaves no detached replay thread parked in `next_tick`/send
    // (E.7). Only after the threads are joined do we drop the last `tx` and let
    // the writer task finish.
    for replay in replays.values() {
        replay.cancel.store(true, Ordering::Relaxed);
    }
    for (_, replay) in replays.drain() {
        quiesce_replay(replay).await;
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
        while let Some(HeldFrame { arrived, frame }) = exec_rx.recv().await {
            let delay = slot.delay_ms.load(Ordering::Relaxed);
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
                    payload: serde_json::to_string(&ServerMessage::Heartbeat {
                        ts_event: sim_now_ns(sim),
                    })
                    .expect("Heartbeat serializes"),
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

/// A live per-symbol replay stream: the cancel flag the handler raises to stop
/// it, the OS-thread handle so the stream can be joined (reaped) rather than
/// detached and left to linger, and the last `ts_event` it successfully sent
/// (or [`NO_TICK_SENT`] if none yet) so a resubscribe of this symbol can seek
/// its replacement strictly past whatever this stream already delivered.
pub(crate) struct Replay {
    pub(crate) cancel: Arc<AtomicBool>,
    pub(crate) handle: std::thread::JoinHandle<()>,
    pub(crate) last_sent_ts: Arc<AtomicU64>,
}

/// Sentinel `last_sent_ts` for a replay that has not yet sent a tick. A real
/// `ts_event` this large is not reachable within the tape's lifetime, so it is
/// distinguishable from any genuine timestamp without an `Option` wrapper.
pub(crate) const NO_TICK_SENT: u64 = u64::MAX;

/// Maximum wall time a replay thread parks while the outbound channel is full
/// before it re-checks its cancel flag. Bounds how long a cancelled stream can
/// stay parked in backpressure, so a quiesce/join completes promptly (E.7).
const REPLAY_SEND_POLL: Duration = Duration::from_millis(5);

/// Maximum wall time one slice of a pacing sleep runs before the replay
/// thread re-checks its cancel flag. Pacing deadlines are unbounded in
/// principle: the accelerated branch deadline-paces every tick with no cap
/// (a session-profile trough or a Subscribe with a future `start_ts` can put
/// the next deadline minutes to days out), and identity mode with
/// `gap_cap_ms = 0` (the documented "0 disables the cap") is just as
/// unbounded. `quiesce_replay` joins the thread inline in the connection's
/// read loop, so one uninterruptible `thread::sleep` that long would stall
/// every Unsubscribe/resubscribe/disconnect for the whole connection until
/// the sleep expired. Slicing the sleep bounds cancel-observation latency to
/// this constant, at a cost of at most ~50 wakeups/sec while pacing -
/// negligible against per-tick synthesis.
const REPLAY_SLEEP_POLL: Duration = Duration::from_millis(20);

/// Sleep until `target_wall_ns` - a unix-ns wall deadline mapped through the
/// replay's NTP-immune `(wall_anchor, instant_anchor)` pairing - in bounded
/// slices of at most [`REPLAY_SLEEP_POLL`], returning early the moment
/// `cancel` is raised. Both pacing branches of `spawn_replay` (accelerated
/// deadline pacing and identity gap pacing) sleep exclusively through this,
/// so a cancelled replay parked mid-gap wakes within one slice instead of
/// holding `quiesce_replay`'s join for the full inter-tick wall gap.
pub(crate) fn sleep_until_wall_cancellable(
    target_wall_ns: u64,
    wall_anchor: u64,
    instant_anchor: Instant,
    cancel: &AtomicBool,
) {
    if target_wall_ns <= wall_anchor {
        return;
    }
    let target = instant_anchor + Duration::from_nanos(target_wall_ns - wall_anchor);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let now = Instant::now();
        if target <= now {
            return;
        }
        std::thread::sleep((target - now).min(REPLAY_SLEEP_POLL));
    }
}

/// Cancel a replay and join its thread, off the async worker.
///
/// The join can block briefly (until the thread observes the cancel between
/// generated ticks, within one [`REPLAY_SEND_POLL`] of a full-channel park, or
/// within one [`REPLAY_SLEEP_POLL`] slice of a pacing sleep), so it runs on a
/// blocking thread rather than stalling the tokio worker driving this
/// connection. Returning only once the thread has exited is what guarantees
/// quiescence: callers rely on it so a replaced stream cannot interleave a stale
/// tick after its successor begins (E.6), and so disconnect reaps every thread.
pub(crate) async fn quiesce_replay(replay: Replay) {
    replay.cancel.store(true, Ordering::Relaxed);
    match tokio::task::spawn_blocking(move || replay.handle.join()).await {
        Ok(Ok(())) => {}
        // The replay OS thread panicked mid-stream. `join()` surfaces the panic
        // payload here; without logging it, a wedged/panicked replay silently
        // ended the feed and the client saw an idle-but-healthy socket rather than
        // any sign the stream died (S15).
        Ok(Err(panic)) => {
            tracing::error!(
                panic = %panic_message(&panic),
                "replay thread panicked; its market-data feed ended silently"
            );
        }
        // The outer spawn_blocking wrapper (not the replay thread) failed to run.
        Err(e) => tracing::warn!(%e, "replay join task panicked"),
    }
}

/// Best-effort human-readable text from a `catch_unwind`/`JoinHandle::join`
/// panic payload - the common `&str` and `String` shapes, else a placeholder.
pub(crate) fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload")
}

/// Quiesce a symbol's in-flight replay and return its resume floor: the last
/// `ts_event` it successfully sent, or `None` if it never sent one.
///
/// The floor is loaded only AFTER the join, and that ordering is load-bearing.
/// The replay thread's cancel checks bracket the send, but a send already past
/// its check completes and stores `last_sent_ts` even if `cancel` lands
/// mid-flight - so between a pre-join load and the thread's exit, one more
/// tick (T2) can enter the shared channel and advance `last_sent_ts` past the
/// loaded value (T1). A floor of T1 makes the replacement seek `T1 + 1` and
/// regenerate everything in `(T1, T2]`: duplicate frames on the wire, breaking
/// the ascending-`ts_event` ordering the adapter's cursor relies on - exactly
/// the E.5/E.6 seam the resume floor exists to close. Once the join returns
/// the thread has exited and `last_sent_ts` is final, so a floor read here
/// covers every tick that could ever have reached the channel.
pub(crate) async fn quiesce_and_resume_floor(old: Replay) -> Option<u64> {
    let last_sent_ts = Arc::clone(&old.last_sent_ts);
    quiesce_replay(old).await;
    let last_sent = last_sent_ts.load(Ordering::Relaxed);
    (last_sent != NO_TICK_SENT).then_some(last_sent)
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

pub(crate) struct ReplaySpawn {
    pub(crate) symbol: String,
    pub(crate) generation: u64,
    pub(crate) start_ts: Option<u64>,
    pub(crate) regime: Option<MarketRegime>,
    pub(crate) speed: f64,
    pub(crate) gap_cap_ms: u64,
    pub(crate) profiles: Arc<source::InstrumentProfiles>,
    pub(crate) sim: SimClock,
    /// The boot-derived tape origin every generator anchors at.
    pub(crate) data_origin: u64,
    pub(crate) tx: mpsc::Sender<Outbound>,
    /// The session's outbound lanes. The thread's diagnostics are admission
    /// truth (`ProtocolError` classifies `EventKind::Admission`), so they ride
    /// the PRIORITY lane - ahead of held traffic and exempt from `DelayAcks` -
    /// instead of the market-data `tx`.
    pub(crate) lanes: ExecLanes,
    /// The promise reserved for this replay's ONE possible diagnostic (the
    /// dead-seek), taken by the handler BEFORE it quiesced whatever stream this
    /// one replaces. Moved into the OS thread so it is released exactly when the
    /// thread exits, whether or not it ever fired. Single-use by type: spending
    /// it consumes it, which is what makes "at most one diagnostic per replay"
    /// a fact rather than a hope. `None` for the direct-`spawn_replay` unit
    /// tests, which do not ration.
    pub(crate) diag_ticket: Option<Ticket>,
    pub(crate) cancel: Arc<AtomicBool>,
    /// The predecessor stream's last successfully-sent `ts_event` for this
    /// symbol, when this spawn replaces one quiesced just now. `None` for a
    /// symbol with no in-flight predecessor (nothing to resume past).
    pub(crate) resume_floor: Option<u64>,
    /// Where this stream records its own last successfully-sent `ts_event`,
    /// read back by the handler if IT is quiesced by a future resubscribe.
    pub(crate) last_sent_ts: Arc<AtomicU64>,
    /// The global replay-capacity permit this stream holds for its whole life
    /// (S22a). Moved into the OS thread so it releases exactly when the thread
    /// exits - whether reaped by a quiesce/join or, defensively, if a handle is
    /// ever dropped without joining. `None` when the cap is not being enforced
    /// for this spawn (the direct-`spawn_replay` unit tests), which simply
    /// never rations.
    pub(crate) permit: Option<OwnedSemaphorePermit>,
}

/// The seek target a replay thread hands to `build_live_source`.
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

/// Stream generated trades for one symbol as market data into `tx`, returning
/// the joinable thread handle so the caller can reap it.
///
/// The replay runs on a dedicated OS thread. It applies backpressure by retrying
/// a `try_send` whenever the channel is full, sleeping at most
/// [`REPLAY_SEND_POLL`] between attempts and re-checking `cancel` each time, so a
/// stream blocked behind a slow/stalled client still observes cancellation
/// promptly instead of parking indefinitely in a plain `blocking_send` (E.7).
pub(crate) fn spawn_replay(spawn: ReplaySpawn) -> std::thread::JoinHandle<()> {
    let ReplaySpawn {
        symbol,
        generation,
        start_ts,
        regime,
        speed,
        gap_cap_ms,
        profiles,
        sim,
        data_origin,
        tx,
        lanes,
        diag_ticket,
        cancel,
        resume_floor,
        last_sent_ts,
        permit,
    } = spawn;
    std::thread::spawn(move || {
        // Held for the thread's whole life and released the instant it exits,
        // so the global replay-capacity pool (S22a) tracks live threads exactly.
        let _permit = permit;
        // Likewise for the promise: unspent, it is returned to the pool when
        // this thread exits, so a healthy replay costs the connection nothing
        // permanent.
        let mut diag_ticket = diag_ticket;
        let symbols = [symbol.clone()];
        // Seek the shared tape to sim-now (or the resume cursor) instead of
        // re-anchoring a fresh generator behind now. Computed here, at the start of
        // the replay thread (~subscribe time), so a fresh subscribe's first live
        // tick lands at sim-now: no accelerated catch-up firehose, no identity
        // backfill replayed at real-time gaps, and a reconnect resumes the same
        // walk at its cursor rather than resetting the price to `start_price`.
        let sim_now = sim.sim_ns(now_ns());
        let seek_start_ts = resume_seek_target(start_ts, resume_floor);
        let Some(mut merged) = source::build_live_source(
            &symbols,
            seek_start_ts,
            regime,
            &profiles,
            data_origin,
            sim_now,
        ) else {
            // No source at all: the symbol is not configured on this venue.
            // Mirror the engine's loud "unknown instrument" order rejection on
            // the data plane - a silent return here left the subscribe
            // indistinguishable from a healthy-but-idle feed.
            tracing::warn!(%symbol, "subscribe for unknown symbol streams nothing");
            // Best-effort diagnostic on the PRIORITY lane, spending this
            // replay's promise. Production cannot reach this branch - the
            // subscribe handler pre-filters unknown symbols (S22), which is
            // what makes ONE promise per replay sufficient - but the guard
            // stays for any other `spawn_replay` caller, and it asserts rather
            // than silently under-covering if that pre-filtering ever regresses.
            debug_assert!(
                diag_ticket.is_some(),
                "an unknown-symbol replay spent its diagnostic promise already"
            );
            spend_diagnostic(
                &lanes,
                &mut diag_ticket,
                generation,
                &symbol,
                SubscriptionIssue::UnknownSymbol,
                sim.sim_ns(now_ns()),
            );
            return;
        };
        tracing::info!(%symbol, ?start_ts, sim_now, data_origin, ?regime, "replay started");

        // The generated tape never runs dry on its own (`GeneratedSource::
        // next_tick` always yields), so a `None` FIRST tick has exactly one
        // meaning: the positioning seek could not reach its target within the
        // budgets - a regime'd from-origin drain past `MAX_HISTORY_SEEK_TICKS`,
        // or a cold checkpoint frontier lagging the target by more than one
        // extension. The stream is dead before it starts; say so on the wire
        // (the untargeted `ProtocolError` exists for exactly this unservable-
        // request vs healthy-but-idle ambiguity) instead of logging started/
        // finished back to back around zero frames.
        let Some(first_tick) = merged.next_tick() else {
            tracing::warn!(
                %symbol,
                ?seek_start_ts,
                sim_now,
                data_origin,
                ?regime,
                budget = source::MAX_HISTORY_SEEK_TICKS,
                "live subscription could not be positioned; the seek exhausted its tick budget"
            );
            // The dead-seek diagnostic: THE one this replay's promise was
            // reserved for, before the handler quiesced whatever stream this
            // one replaced.
            spend_diagnostic(
                &lanes,
                &mut diag_ticket,
                generation,
                &symbol,
                SubscriptionIssue::SeekBudgetExhausted,
                sim.sim_ns(now_ns()),
            );
            return;
        };

        // Every pacing deadline below is anchored to ONE monotonic `Instant`,
        // paired with ONE wall-clock read, taken here at thread start. Each
        // branch converts a wall-clock deadline (unix-nanos) into an offset from
        // this pairing and sleeps against the `Instant` rather than re-reading
        // the wall clock per tick - which is what makes pacing immune to an
        // NTP/leap step mid-session: nothing after this line consults
        // `CLOCK_REALTIME` again, so a backward step can no longer make the
        // feed stall (a huge `deadline - now`) and a forward step can no longer
        // make it burst (`deadline <= now` unpaced). The sleep itself is the
        // sliced, cancel-aware `sleep_until_wall_cancellable`, so an unbounded
        // pacing gap cannot hold a quiesce hostage; a sleep cut short by cancel
        // falls through to the pre-send cancel check below and exits before
        // emitting the tick it was pacing.
        let wall_anchor = now_ns();
        let instant_anchor = Instant::now();
        let sleep_until_wall = |target_wall_ns: u64| {
            sleep_until_wall_cancellable(target_wall_ns, wall_anchor, instant_anchor, &cancel);
        };

        // Identity pacing (the `else if speed > 0.0` branch below) sleeps the
        // inter-tick gap measured from the PREVIOUS tick; with no previous tick
        // the first one emits with no sleep. For a fresh subscribe - which seeks
        // the tape to sim-now - the first generated tick lands a few seconds PAST
        // sim-now, so an unpaced emit would hand the client a tick stamped ahead
        // of the clock. Seed the pacer with the seek target (sim-now) so the first
        // tick is paced to its own timestamp instead of racing out immediately.
        // The accelerated branch deadline-paces every tick (including the first)
        // and ignores `prev_ts`, so this only affects identity mode. An explicit
        // `start_ts` is a historical window or resume cursor that lands at or
        // before its target - that is backfill and should replay promptly, so it
        // keeps the `None` seed and the first tick emits at once.
        let mut prev_ts: Option<u64> = start_ts.is_none().then_some(sim_now);
        // Accumulates the identity/speed branch's absolute schedule as
        // `wall_anchor`-relative deadlines, rather than resetting a fresh
        // relative sleep off every tick. A per-tick relative sleep never
        // accounts for the time already spent generating/sending the previous
        // tick, so over a long session the realized spacing (gap + generation +
        // scheduling) would progressively lag real wall-clock time. Chaining
        // deadlines instead - each the last deadline plus this tick's wait -
        // removes that drift: a slow iteration simply arrives at an
        // already-past deadline and does not sleep, rather than pushing every
        // later deadline back by the overrun.
        let mut next_deadline_wall: Option<u64> = None;
        // `next` starts as the tick pulled by the dead-subscribe probe above,
        // then follows the merge; the pacing and send below treat it exactly
        // as the old loop-top `next_tick` did.
        let mut next = Some(first_tick);
        while let Some(tick) = next {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            if !sim.is_identity() {
                sleep_until_wall(sim.wall_ns(tick.ts_event()));
            } else if speed > 0.0 {
                if let Some(prev) = prev_ts {
                    let gap_ns = tick.ts_event().saturating_sub(prev);
                    // Pace at nanosecond resolution: integer-dividing the scaled
                    // gap down to whole milliseconds collapses any sub-ms
                    // inter-tick gap to a zero-delay send, so micros-apart bursts
                    // (which the generator emits) wouldn't be paced at all and the
                    // realized timeline would not track original_timeline / speed.
                    let mut wait_ns = (gap_ns as f64 / speed) as u64;
                    if gap_cap_ms > 0 {
                        wait_ns = wait_ns.min(gap_cap_ms.saturating_mul(1_000_000));
                    }
                    let deadline = next_deadline_wall
                        .unwrap_or(wall_anchor)
                        .saturating_add(wait_ns);
                    sleep_until_wall(deadline);
                    next_deadline_wall = Some(deadline);
                }
                prev_ts = Some(tick.ts_event());
            }

            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let ts_event = tick.ts_event();
            let msg = match tick {
                TickEvent::Trade(t) => ServerMessage::Trade(t),
                TickEvent::Quote(q) => ServerMessage::Quote(q),
            };
            let payload = match serde_json::to_string(&msg) {
                Ok(payload) => payload,
                Err(e) => {
                    tracing::error!(%e, "could not serialize market-data frame");
                    break;
                }
            };
            if send_cancellable(
                &tx,
                Outbound::Frame(OutboundFrame {
                    payload,
                    is_market_data: true,
                    charge: None,
                    slot: None,
                }),
                &cancel,
            )
            .is_err()
            {
                break; // client gone or stream cancelled
            }
            last_sent_ts.store(ts_event, Ordering::Relaxed);
            next = merged.next_tick();
        }
        tracing::info!(%symbol, "replay finished");
    })
}

/// Send `msg` into `tx`, applying backpressure without parking indefinitely.
///
/// `Sender::blocking_send` would block until the channel drains, and the replay
/// thread would only re-check `cancel` at the next loop top - so a stream behind
/// a stalled client could sit unkillable in a full channel. Instead this retries
/// `try_send`, sleeping at most [`REPLAY_SEND_POLL`] when the channel is full and
/// re-checking `cancel` between attempts. `Err(())` means the stream should stop
/// (client gone, or cancelled while parked).
fn send_cancellable<T>(tx: &mpsc::Sender<T>, msg: T, cancel: &AtomicBool) -> Result<(), ()> {
    use mpsc::error::TrySendError;
    let mut msg = msg;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(());
        }
        match tx.try_send(msg) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Full(returned)) => {
                msg = returned;
                std::thread::sleep(REPLAY_SEND_POLL);
            }
            Err(TrySendError::Closed(_)) => return Err(()),
        }
    }
}
