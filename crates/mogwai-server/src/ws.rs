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
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use mogwai_data::TickEvent;
use mogwai_protocol::{ClientMessage, MarketRegime, ServerMessage, SimClock};
use tokio::sync::{OwnedSemaphorePermit, mpsc};

use crate::config::{now_ns, sim_duration_from_millis, sim_now_ns};
use crate::http::{
    AppState, process_order_cmd, strip_unfireable_reopen_gap, validate_regime_or_clean,
};
use crate::source;

pub(crate) async fn ws_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Emit a `ProtocolError` diagnostic through the execution-delay pump rather
/// than straight onto the writer channel. `ServerMessage::category` classifies
/// `ProtocolError` as execution, and havoc.md says `DelayAcks` holds EVERY
/// outbound execution event, so routing it here is what makes the route match
/// the classification (S10): an armed `DelayAcks` now holds these diagnostics
/// too, and the writer's `GoDark` gate still applies downstream. `drop(...await)`
/// because a send error just means the writer already gave up on this socket -
/// the read loop observes the same and exits.
async fn send_exec_protocol_error(
    exec_tx: &mpsc::Sender<(Instant, ServerMessage)>,
    ts_event: u64,
    reason: String,
) {
    drop(
        exec_tx
            .send((
                Instant::now(),
                ServerMessage::ProtocolError { reason, ts_event },
            ))
            .await,
    );
}

/// Reconcile a live `Subscribe`'s `start_ts` against the tape bounds, mirroring
/// the `/trades` handler's refusals but as a DEGRADATION - the live feed still
/// starts, because a subscribe carries the client's real want (the feed itself).
/// Either shortfall goes out as a `ProtocolError` diagnostic so a healthy-looking
/// feed cannot hide it. Returns the effective `start_ts` to hand the replay.
pub(crate) async fn reconcile_subscribe_start_ts(
    start_ts: Option<u64>,
    state: &AppState,
    exec_tx: &mpsc::Sender<(Instant, ServerMessage)>,
) -> Option<u64> {
    let ts = start_ts?;
    // Below the tape origin: `/trades` refuses the identical window with a 422,
    // but here the stream is kept and the generator anchors at the origin
    // naturally (its first tick lands at or after the origin) - only announce the
    // shortfall. `start_ts` passes through unchanged.
    if ts < state.data_origin_ns {
        tracing::warn!(
            start_ts = ts,
            data_origin = state.data_origin_ns,
            "subscribe start_ts precedes the tape origin; stream anchors at the origin"
        );
        send_exec_protocol_error(
            exec_tx,
            sim_now_ns(state.sim),
            format!(
                "subscribe start_ts {ts} precedes data_origin_ns {}; \
                 the tape begins at its origin and the stream anchors there",
                state.data_origin_ns
            ),
        )
        .await;
        return start_ts;
    }
    // Beyond sim-now: the WS twin of the `/trades` future refusal (F8). Honoring
    // it as given would extend the shared index into the future and, in identity
    // mode, emit an unpaced look-ahead first tick stamped ahead of the clock -
    // exactly what `/trades` 422s. Consistent with the below-origin degradation
    // above, clamp to a fresh live stream from the clock: returning `None` makes
    // the replay seek sim-now and seed its pacer there, so the first tick is
    // paced like any live subscribe, and resumes past a quiesced predecessor
    // rather than jumping to the future. Announce the clamp.
    let sim_now = sim_now_ns(state.sim);
    if ts > sim_now {
        tracing::warn!(
            start_ts = ts,
            sim_now,
            "subscribe start_ts exceeds sim-now; clamping to a live stream from the clock"
        );
        send_exec_protocol_error(
            exec_tx,
            sim_now,
            format!(
                "subscribe start_ts {ts} exceeds sim-now {sim_now}; \
                 the tape cannot serve past the clock, streaming live from now"
            ),
        )
        .await;
        return None;
    }
    start_ts
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
async fn handle_socket(socket: WebSocket, state: AppState) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::channel::<ServerMessage>(1024);
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
    let dark_until_ns = Arc::clone(&state.dark_until_ns);
    let stall_until_ns = Arc::clone(&state.stall_until_ns);
    let sim = state.sim;

    // `tx`/`rx` is the single channel the writer below drains and serializes
    // onto the socket - market data (replay threads, heartbeat) sends land in
    // it directly. Execution events take one extra hop through `exec_tx`/
    // `exec_pump` so a `DelayAcks` sleep never blocks the writer's own
    // `rx.recv()` loop (see `spawn_exec_pump`). Each event rides with the
    // `Instant` it was enqueued at, so the pump can anchor its delay deadline
    // at production time rather than at whenever it gets around to dequeuing.
    let (exec_tx, exec_rx) = mpsc::channel::<(Instant, ServerMessage)>(1024);

    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let now = sim_now_ns(sim);
            if now < dark_until_ns.load(Ordering::Relaxed) {
                continue;
            }
            if msg.is_market_data() && now < stall_until_ns.load(Ordering::Relaxed) {
                continue;
            }
            // Skip an un-serializable frame rather than panicking the writer
            // task: this runs in a detached `tokio::spawn`, so an `expect` here
            // would silently tear down the whole connection's outbound stream.
            let payload = match serde_json::to_string(&msg) {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!(%e, "dropping un-serializable ServerMessage");
                    continue;
                }
            };
            if sink.send(Message::Text(payload.into())).await.is_err() {
                break; // client gone
            }
        }
    });

    let exec_pump = spawn_exec_pump(exec_rx, Arc::clone(&state.delay_ms), state.sim, tx.clone());

    let heartbeat = if state.cfg.server_heartbeat_ms > 0 {
        Some(spawn_heartbeat(
            state.cfg.server_heartbeat_ms,
            state.sim,
            tx.clone(),
        ))
    } else {
        None
    };

    while let Some(Ok(msg)) = stream.next().await {
        let Message::Text(text) = msg else { continue };

        let client_msg: ClientMessage = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(%e, %text, "undecodable client message");
                // Surface the decode failure on the wire instead of dropping it
                // silently: previously a malformed/under-specified frame (e.g.
                // `{"type":"Subscribe"}` missing `symbols`) looked identical to a
                // healthy-but-idle feed. Routed through the exec pump so this
                // execution-category diagnostic honors DelayAcks like every other
                // execution event (S10).
                send_exec_protocol_error(&exec_tx, sim_now_ns(state.sim), e.to_string()).await;
                continue;
            }
        };

        match client_msg {
            ClientMessage::Subscribe {
                symbols,
                start_ts,
                regime,
            } => {
                let mut regime = validate_regime_or_clean(regime);
                // A ReopenGap anchored at or before the tape origin can never
                // fire; strip it (D3) and say so on the wire - the stream
                // still starts, serving the clean tape the generator would
                // have realized anyway.
                if let Some(at_ts) = strip_unfireable_reopen_gap(&mut regime, state.data_origin_ns)
                {
                    send_exec_protocol_error(
                        &exec_tx,
                        sim_now_ns(state.sim),
                        format!(
                            "ReopenGap at_ts {at_ts} is at or before data_origin_ns {}; \
                             the halt can never fire, streaming the clean tape",
                            state.data_origin_ns
                        ),
                    )
                    .await;
                }
                // Reconcile the requested window against the tape bounds up front:
                // a start below the origin anchors at the origin, a start beyond
                // sim-now clamps to a live stream from the clock (F8), each with a
                // ProtocolError diagnostic. The effective start_ts drives every
                // symbol's replay below.
                let start_ts = reconcile_subscribe_start_ts(start_ts, &state, &exec_tx).await;
                for symbol in dedup_symbols(symbols) {
                    // Unknown symbols get their diagnostic here rather than from a
                    // spawned replay thread: whether the venue lists a symbol is a
                    // cheap synchronous check, so there is no reason to spin up an
                    // OS thread that immediately exits and leaves a dead entry in
                    // `replays` (S22). The dead-SEEK diagnostic - knowable only by
                    // actually running the positioning seek - still comes from the
                    // thread in `spawn_replay`.
                    if state.profiles.get(&symbol).is_none() {
                        tracing::warn!(%symbol, "subscribe for unknown symbol streams nothing");
                        send_exec_protocol_error(
                            &exec_tx,
                            sim_now_ns(state.sim),
                            format!(
                                "subscribe for unknown symbol {symbol}: the venue does not \
                                 list it, no data will stream"
                            ),
                        )
                        .await;
                        continue;
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
                            send_exec_protocol_error(
                                &exec_tx,
                                sim_now_ns(state.sim),
                                format!(
                                    "replay capacity reached ({} concurrent streams); \
                                     symbol {symbol} not started",
                                    state.cfg.max_concurrent_replays
                                ),
                            )
                            .await;
                            continue;
                        }
                    };
                    let cancel = Arc::new(AtomicBool::new(false));
                    let last_sent_ts = Arc::new(AtomicU64::new(NO_TICK_SENT));
                    let handle = spawn_replay(ReplaySpawn {
                        symbol: symbol.clone(),
                        start_ts,
                        regime,
                        speed: state.cfg.speed,
                        gap_cap_ms: state.cfg.gap_cap_ms,
                        profiles: Arc::clone(&state.profiles),
                        sim: state.sim,
                        data_origin: state.data_origin_ns,
                        tx: tx.clone(),
                        exec_tx: exec_tx.clone(),
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
            }
            ClientMessage::Unsubscribe { symbols } => {
                for symbol in dedup_symbols(symbols) {
                    if let Some(old) = replays.remove(&symbol) {
                        quiesce_replay(old).await;
                    }
                }
            }
            order_cmd => {
                let events = process_order_cmd(order_cmd, &state).await;
                // One arrival instant for the whole batch: the engine produced
                // these events together in one `process` call, so under an
                // armed `DelayAcks` they share one deadline and land together
                // ~ms late (see `spawn_exec_pump` for the per-event-deadline
                // contract this anchors).
                let arrived = Instant::now();
                for ev in events {
                    debug_assert!(
                        is_execution_event(&ev),
                        "order-entry events are always execution-category, never market data"
                    );
                    if exec_tx.send((arrived, ev)).await.is_err() {
                        break;
                    }
                }
            }
        }
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
    if let Err(e) = writer.await {
        tracing::warn!(%e, "writer task did not shut down cleanly");
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
    mut exec_rx: mpsc::Receiver<(Instant, ServerMessage)>,
    delay_ms: Arc<AtomicU64>,
    sim: SimClock,
    out: mpsc::Sender<ServerMessage>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some((arrived, msg)) = exec_rx.recv().await {
            let delay = delay_ms.load(Ordering::Relaxed);
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
            if out.send(msg).await.is_err() {
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
    tx: mpsc::Sender<ServerMessage>,
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
            if tx
                .send(ServerMessage::Heartbeat {
                    ts_event: sim_now_ns(sim),
                })
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

pub(crate) fn is_execution_event(msg: &ServerMessage) -> bool {
    // Delegates to the protocol's shared classifier (`ServerMessage::category`)
    // so the server's `DelayAcks` delay path and the adapter's inbound latency
    // bucketing decide exec-vs-data from one source of truth. Execution traffic
    // is everything but market data - notably `AccountState`, an account event
    // that reports balances and positions moved by fills, which both ends now
    // agree rides the execution path.
    msg.category().is_execution()
}

pub(crate) struct ReplaySpawn {
    pub(crate) symbol: String,
    pub(crate) start_ts: Option<u64>,
    pub(crate) regime: Option<MarketRegime>,
    pub(crate) speed: f64,
    pub(crate) gap_cap_ms: u64,
    pub(crate) profiles: Arc<source::InstrumentProfiles>,
    pub(crate) sim: SimClock,
    /// The boot-derived tape origin every generator anchors at.
    pub(crate) data_origin: u64,
    pub(crate) tx: mpsc::Sender<ServerMessage>,
    /// The session's execution-delay pump. `ProtocolError` classifies as
    /// execution (`ServerMessage::category`), so the thread's diagnostics ride
    /// this channel - where an armed `DelayAcks` holds them - instead of the
    /// market-data `tx` (S10).
    pub(crate) exec_tx: mpsc::Sender<(Instant, ServerMessage)>,
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
        start_ts,
        regime,
        speed,
        gap_cap_ms,
        profiles,
        sim,
        data_origin,
        tx,
        exec_tx,
        cancel,
        resume_floor,
        last_sent_ts,
        permit,
    } = spawn;
    std::thread::spawn(move || {
        // Held for the thread's whole life and released the instant it exits,
        // so the global replay-capacity pool (S22a) tracks live threads exactly.
        let _permit = permit;
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
            // Best-effort diagnostic: an Err means the client is already gone
            // (channel closed or subscription cancelled), so there is nobody
            // left to tell - the thread exits either way. Rides the exec pump
            // (not `tx`) so DelayAcks holds it like every execution event
            // (S10). Production cannot reach this branch anymore - the
            // subscribe handler pre-filters unknown symbols (S22) - but the
            // guard stays for any other `spawn_replay` caller.
            if send_cancellable(
                &exec_tx,
                (
                    Instant::now(),
                    ServerMessage::ProtocolError {
                        reason: format!(
                            "subscribe for unknown symbol {symbol}: the venue does not list it, \
                             no data will stream"
                        ),
                        ts_event: sim.sim_ns(now_ns()),
                    },
                ),
                &cancel,
            )
            .is_err()
            {
                tracing::debug!(%symbol, "protocol error frame undeliverable; client gone");
            }
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
            // Best-effort diagnostic: an Err means the client is already gone
            // (channel closed or subscription cancelled), so there is nobody
            // left to tell - the thread exits either way. Rides the exec pump
            // (not `tx`) so DelayAcks holds it like every execution event (S10).
            if send_cancellable(
                &exec_tx,
                (
                    Instant::now(),
                    ServerMessage::ProtocolError {
                        reason: format!(
                            "subscription for {symbol} could not be positioned: the seek toward \
                             its start exhausted the {}-tick budget, no data will stream",
                            source::MAX_HISTORY_SEEK_TICKS
                        ),
                        ts_event: sim.sim_ns(now_ns()),
                    },
                ),
                &cancel,
            )
            .is_err()
            {
                tracing::debug!(%symbol, "protocol error frame undeliverable; client gone");
            }
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
            if send_cancellable(&tx, msg, &cancel).is_err() {
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
