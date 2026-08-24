// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use mogwai_protocol::{Command, ConnHavoc, SimClock, VenueMessage};
use rand::{RngExt, SeedableRng, rngs::StdRng};
use tokio::{
    sync::{
        Mutex,
        mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
    },
    time::{Instant, MissedTickBehavior, Sleep},
};
use tokio_tungstenite::{connect_async, tungstenite::Message};

/// A socket generation owns its split reader and writer tasks. Dropping the
/// outer connection future must cancel them too; dropping a bare JoinHandle
/// detaches the task and leaks both socket halves.
struct AbortOnDrop(Option<tokio::task::JoinHandle<()>>);

impl AbortOnDrop {
    fn new(handle: tokio::task::JoinHandle<()>) -> Self {
        Self(Some(handle))
    }

    async fn abort_and_join(mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
            drop(handle.await);
        }
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if let Some(handle) = &self.0 {
            handle.abort();
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ReconnectPolicy {
    initial_ms: u64,
    max_ms: u64,
    factor: f64,
    jitter_ms: u64,
    max_attempts: Option<u32>,
    sim: SimClock,
}

impl ReconnectPolicy {
    pub(crate) fn from_conn(conn: &ConnHavoc, sim: SimClock) -> Self {
        Self {
            initial_ms: conn.reconnect_delay_initial_ms,
            max_ms: conn.reconnect_delay_max_ms,
            factor: conn.reconnect_backoff_factor,
            jitter_ms: conn.reconnect_jitter_ms,
            max_attempts: conn.reconnect_max_attempts,
            sim,
        }
    }

    pub(crate) fn backoff(&self, attempt: u32, rng: &mut StdRng) -> Duration {
        let initial_ms = self.initial_ms as f64;
        let max_ms = self.max_ms as f64;
        // `max_ms == 0` means "no ceiling": clamp only when a positive max is
        // configured. The old code collapsed the delay to zero whenever either
        // bound was zero, so a nonzero initial paired with a zero max busy-spun
        // the reconnect loop (D.3). The protocol validator now rejects both
        // mixed-zero combinations - a positive initial with a zero max, and
        // (because the base is `initial * factor^attempt`) a zero initial with
        // a positive max, the same spin from the other side - so the only
        // validated config that yields a zero delay here is both bounds zero:
        // backoff deliberately disabled, nothing to spin against. The `max ==
        // 0` branch below stays as belt-and-suspenders for a caller that
        // bypasses the validator; the mirror case (zero initial, positive max)
        // deliberately gets no defensive floor - the mechanism has no
        // principled delay to invent, and silently substituting one would mask
        // the missing validation instead of surfacing it.
        let uncapped = initial_ms * self.factor.powi(attempt as i32);
        let base_ms = if max_ms == 0.0 {
            uncapped
        } else {
            uncapped.min(max_ms)
        };
        let jitter = if self.jitter_ms == 0 {
            0
        } else {
            rng.random_range(0..=self.jitter_ms)
        };
        let base_ms = base_ms.max(0.0).min(u64::MAX as f64) as u64;
        let total_ms = base_ms.saturating_add(jitter);
        if total_ms == 0 {
            Duration::ZERO
        } else {
            self.sim.wall_duration(total_ms.saturating_mul(1_000_000))
        }
    }

    pub(crate) fn exhausted(&self, attempt: u32) -> bool {
        self.max_attempts.is_some_and(|max| attempt >= max)
    }
}

/// Backs off before the next reconnect, unless the attempt cap is already
/// spent. Computes the backoff for the current `*attempt`, advances the
/// counter, and returns whether the counter, once advanced, has hit
/// `reconnect_max_attempts`. When it has, the sleep is skipped and `true` is
/// returned: the caller must return instead of looping, because the loop-top
/// exhausted check would return anyway and sleeping a final backoff first only
/// delays that exhausted return by one pointless interval (F12). When the cap
/// is not yet reached, the current attempt's backoff is slept and `false` is
/// returned so the caller re-dials. `connected` is already stored `false` on
/// both reconnect paths, so an exhausted return needs no further store here.
///
/// Both outcomes log: exhaustion permanently stops the socket (the loop
/// returns and nothing restarts it), which without an `error` line looks
/// identical to a quiet venue, and the backoff `info` line is what makes a
/// reconnect-in-progress readable in the worker log at all - a venue outage
/// used to leave zero connection-lifecycle lines while the client silently
/// re-attached.
async fn backoff_or_exhausted(
    policy: &ReconnectPolicy,
    attempt: &mut u32,
    rng: &mut StdRng,
    label: &'static str,
) -> bool {
    let delay = policy.backoff(*attempt, rng);
    *attempt = attempt.saturating_add(1);
    if policy.exhausted(*attempt) {
        tracing::error!(
            socket = label,
            attempts = *attempt,
            "reconnect attempts exhausted; giving up on the venue websocket"
        );
        return true;
    }
    tracing::info!(
        socket = label,
        attempt = *attempt,
        delay_ms = delay.as_millis() as u64,
        "venue websocket down; backing off before reconnect"
    );
    tokio::time::sleep(delay).await;
    false
}

/// Per-connection HTTP rate limiter enforcing `max_requests_per_second`.
///
/// Accounting contract - which HTTP calls this meters, and which are exempt
/// (AE13). Keep call sites consistent with this list, since the meter itself
/// cannot see who bypasses it:
///
/// Metered (the recurring HTTP calls either client makes). Every
/// `fetch_instruments` (`GET /instruments`, including the reseeds
/// `ensure_instrument` triggers), every `fetch_account` (`GET /account`) and
/// every per-dial run-identity probe (`GET /health`) awaits `wait()` before
/// issuing its request, so the configured ceiling governs the ongoing request
/// stream a running strategy generates. Market data, history and order entry
/// no longer pass here at all: they are websocket traffic, and this meter never
/// sees them.
///
/// Exempt (connect-time bootstrap, deliberately un-metered):
///   - the clock fetch (`clock::fetch_clock`) - it runs before this quota
///     exists, because the quota's spacing is `sim`-scaled and `sim` is exactly
///     what the clock fetch returns; `connect()` builds the quota from that
///     result. Metering it is a chicken-and-egg: there is no quota to wait on
///     yet.
///   - `ship_venue_havoc` - a bounded, one-shot control-plane POST loop that
///     arms divergences at connect, not part of the steady request stream.
///
/// Both exemptions are connect-time and bounded, so neither contributes to the
/// sustained rate the ceiling exists to bound. Routing them through the meter
/// too would be strictly consistent but is a call-site change in
/// `client/data.rs` and `client/exec.rs`, not something this type can enforce.
///
/// Known shape, not fixed here: `wait()` holds the mutex across its sleep to
/// enforce FIFO spacing across concurrent callers, so a burst of N fetches
/// queues linearly with no cap and no timeout. What that delays is an
/// instrument reseed, an account pull or a reconnect's identity probe - a
/// subscription whose def arrives late, or a redial that waits its turn -
/// never an order, which does not pass this meter. A queue cap/timeout would be
/// a design change; the spacing-via-held-mutex is intentional.
#[derive(Clone, Debug)]
pub(crate) struct HttpQuota {
    min_interval: Option<Duration>,
    last_send: Arc<Mutex<Option<Instant>>>,
}

impl HttpQuota {
    pub(crate) fn from_conn(conn: &ConnHavoc, sim: SimClock) -> Self {
        Self {
            min_interval: conn.max_requests_per_second.map(|max| {
                // Round the per-request spacing up (ceil-divide) so a rate that
                // does not evenly divide one second (e.g. 3/s -> 333_333_334ns,
                // not the floored 333_333_333 that ships a hair fast) never
                // undershoots the spacing and ships marginally above the cap
                // (D.9). Ceil keeps the effective rate at or below the
                // configured ceiling.
                let max = u64::from(max);
                let nanos = crate::clock::NANOS_PER_SEC.div_ceil(max);
                sim.wall_duration(nanos.max(1))
            }),
            last_send: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) async fn wait(&self) {
        let Some(min_interval) = self.min_interval else {
            return;
        };
        let mut last_send = self.last_send.lock().await;
        if let Some(last) = *last_send {
            let elapsed = last.elapsed();
            if elapsed < min_interval {
                tokio::time::sleep(min_interval - elapsed).await;
            }
        }
        *last_send = Some(Instant::now());
    }
}

enum WsAction<Cmd> {
    Inbound(Option<Result<Message, tokio_tungstenite::tungstenite::Error>>),
    Command(Option<Cmd>),
    Heartbeat,
    Idle,
}

pub(crate) struct WsConnectionConfig {
    pub(crate) ws_url: String,
    pub(crate) conn: ConnHavoc,
    pub(crate) seed: Option<u64>,
    pub(crate) connected: Arc<AtomicBool>,
    pub(crate) connected_notify: Arc<tokio::sync::Notify>,
    pub(crate) sim: SimClock,
    /// Names this socket in the connection-lifecycle log lines ("data" /
    /// "exec"). A venue outage hits both sockets concurrently, so without the
    /// tag the interleaved disconnect/backoff/reconnect lines cannot be told
    /// apart.
    pub(crate) label: &'static str,
    /// Proves, on every dial, that the thing answering is still this client's
    /// run. `None` dials blind, which is the historical behaviour.
    ///
    /// It lives here rather than in either client's connect path because the
    /// Reconnect is the exposure: a re-dial never re-runs that path, so a check
    /// placed there would cover only the first connection - the one least likely
    /// to reach a stranger.
    pub(crate) identity: Option<RunIdentityCheck>,
    /// How long one dial may take before it is abandoned and retried.
    ///
    /// It bounds every single dial, not just the first, which is what the name has to
    /// mean if a host is going to set it. A first boarding on a cold river pays
    /// that river's whole warmup synthesis inside the upgrade - the venue
    /// materializes no river before something asks for one - so this is the
    /// deadline a large warmup runs into. A reconnect to a river already
    /// materialized is cheap, but a reconnect after its boat wound down, or one
    /// that follows a first dial which never completed placement, pays the same
    /// cost as a first.
    pub(crate) dial_timeout: Duration,
}

/// What an identity probe established. Three outcomes, not two: only one of
/// them refuses the connection, and the other two are refused-to-refuse for
/// different reasons that must not be reported as each other.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum IdentityOutcome {
    /// The venue named this client's run.
    Confirmed,
    /// No usable answer - a transport error, an error status, or a body that is
    /// not JSON. Indistinguishable from the socket failing the same way.
    Unreachable(String),
    /// Answered, well-formed, and carrying no run identity at all. This is
    /// version skew, not a transport failure: the venue predates run identity.
    /// It was filed under `Unreachable` once, which made a real skew
    /// undiscoverable by anyone grepping for the category it was filed under.
    Unidentified(String),
    /// Answered, by a different run.
    Mismatch(String),
}

/// Sorts a probe result into its outcome.
///
/// Split out so the classification is testable without a socket - the bug this
/// exists to prevent was a correctly-worded detail filed under a contradicting
/// headline, which no black-box assertion on behaviour would have caught,
/// because all three non-mismatch paths behave identically.
pub(crate) fn classify_identity(result: Result<(), String>) -> IdentityOutcome {
    match result {
        Ok(()) => IdentityOutcome::Confirmed,
        Err(reason) if reason.starts_with(IDENTITY_UNREACHABLE) => {
            IdentityOutcome::Unreachable(reason)
        }
        Err(reason) if reason.starts_with(IDENTITY_NOT_REPORTED) => {
            IdentityOutcome::Unidentified(reason)
        }
        Err(reason) => IdentityOutcome::Mismatch(reason),
    }
}

/// Runs the identity check, turning "could not ask" and "asked, got no answer
/// to this question" into distinct outcomes from "asked, and it is someone
/// else".
///
/// Neither non-answer is a mismatch. A probe can fail for the same transport
/// reasons the socket can, and refusing terminally on that would turn a blip
/// into a dead client; a venue too old to report a run cannot be judged by a
/// check it does not implement. Only a venue that answers, as a different run,
/// earns the refusal.
async fn verify_run_identity(
    check: &(
         dyn Fn() -> std::pin::Pin<Box<dyn Future<Output = Result<(), String>> + Send>>
             + Send
             + Sync
     ),
    label: &'static str,
) -> Result<(), String> {
    match classify_identity(check().await) {
        IdentityOutcome::Confirmed => Ok(()),
        IdentityOutcome::Unreachable(reason) => {
            tracing::warn!(
                socket = label,
                %reason,
                "could not reach this venue to confirm which run it is serving; proceeding, \
                 because an unanswered probe is a transport failure rather than a wrong venue"
            );
            Ok(())
        }
        IdentityOutcome::Unidentified(reason) => {
            tracing::warn!(
                socket = label,
                %reason,
                "this venue does not report a run at all, so this client cannot verify it is \
                 the one it was launched against; proceeding, but the venue and this build are \
                 out of step - rebuild the older one to get the check back"
            );
            Ok(())
        }
        IdentityOutcome::Mismatch(reason) => Err(reason),
    }
}

/// Prefix marking an identity probe that got no usable answer - the request
/// failed, or returned an error status, or returned something that is not JSON.
pub(crate) const IDENTITY_UNREACHABLE: &str = "unreachable: ";

/// Prefix marking an identity probe that was answered, correctly, by a venue
/// that reports no run at all. Distinct from [`IDENTITY_UNREACHABLE`] because
/// nothing failed: this is version skew, and calling it a transport failure
/// sends whoever reads the log looking for a network problem that is not there.
pub(crate) const IDENTITY_NOT_REPORTED: &str = "unidentified: ";

/// Asks the venue at this address which run it is serving.
///
/// Boxed rather than generic because the connection loop is already carrying
/// seven type parameters, and this one is called once per dial on a path whose
/// cost is a network round trip.
pub(crate) type RunIdentityCheck = std::sync::Arc<
    dyn Fn() -> std::pin::Pin<Box<dyn Future<Output = Result<(), String>> + Send>> + Send + Sync,
>;

/// Drives one client socket for the life of the client: dial, reattach,
/// pump, disconnect, redial.
///
/// `on_undelivered` is never optional machinery, and the reason is the whole of
/// finding 5. `send_command` pushes onto an unbounded channel a separate writer
/// task drains, so an `Ok` from it means accepted for writing and nothing more.
/// When the socket drops, the writer is aborted with whatever it had not yet
/// written, and a submit, cancel, modify or query in that window used to vanish
/// with no error anywhere: the caller had already been told `Ok`, so nothing
/// synthesized a terminal event and the order sat `Submitted` forever.
///
/// So every command that was accepted and never written is handed back through
/// this callback, exactly once, in the order it was queued. This adapter
/// deliberately does not replay them onto the next generation's socket:
/// re-submitting orders across a reconnect is a policy call a venue adapter has
/// no business making silently on a host's behalf. Telling the caller is not
/// optional; deciding for it is.
pub(crate) async fn run_ws_connection<
    Cmd,
    Serialize,
    OnConnect,
    Handler,
    HandlerFut,
    Disconnect,
    DisconnectFut,
    OnUndelivered,
>(
    config: WsConnectionConfig,
    cmd_rx: Option<UnboundedReceiver<Cmd>>,
    serialize: Serialize,
    on_connect: OnConnect,
    handler: Handler,
    on_disconnect: Disconnect,
    mut on_undelivered: OnUndelivered,
) where
    Cmd: Send + 'static,
    Serialize: Fn(&Cmd) -> Command + Send + Sync + 'static,
    OnConnect: Fn() -> Vec<Cmd> + Send + Sync + 'static,
    Handler: FnMut(VenueMessage) -> HandlerFut + Send + 'static,
    HandlerFut: Future<Output = ()> + Send,
    Disconnect: FnMut() -> DisconnectFut + Send + 'static,
    DisconnectFut: Future<Output = ()> + Send,
    OnUndelivered: FnMut(Cmd) + Send + 'static,
{
    let mut cmd_rx = cmd_rx;
    run_ws_connection_inner(
        config,
        &mut cmd_rx,
        serialize,
        on_connect,
        handler,
        on_disconnect,
        &mut on_undelivered,
    )
    .await;
    // The loop's own exit is an undelivery too. Whatever is still sitting in the
    // command receiver when this returns - the run completed, the attempt cap
    // tripped, the identity check refused - is never going to be written by
    // anybody, because the receiver dies with this frame. Commands sent after
    // that point need no report here: the sender sees a closed channel and
    // `dispatch_order` already synthesizes for that case. This drain covers the
    // window between the last write and the receiver's death, which nothing
    // else can see.
    if let Some(rx) = cmd_rx.as_mut() {
        rx.close();
        while let Ok(cmd) = rx.try_recv() {
            on_undelivered(cmd);
        }
    }
}

async fn run_ws_connection_inner<
    Cmd,
    Serialize,
    OnConnect,
    Handler,
    HandlerFut,
    Disconnect,
    DisconnectFut,
    OnUndelivered,
>(
    config: WsConnectionConfig,
    cmd_rx: &mut Option<UnboundedReceiver<Cmd>>,
    serialize: Serialize,
    on_connect: OnConnect,
    mut handler: Handler,
    mut on_disconnect: Disconnect,
    on_undelivered: &mut OnUndelivered,
) where
    Cmd: Send + 'static,
    Serialize: Fn(&Cmd) -> Command + Send + Sync + 'static,
    OnConnect: Fn() -> Vec<Cmd> + Send + Sync + 'static,
    Handler: FnMut(VenueMessage) -> HandlerFut + Send + 'static,
    HandlerFut: Future<Output = ()> + Send,
    Disconnect: FnMut() -> DisconnectFut + Send + 'static,
    DisconnectFut: Future<Output = ()> + Send,
    OnUndelivered: FnMut(Cmd) + Send + 'static,
{
    let WsConnectionConfig {
        ws_url,
        conn,
        seed,
        connected,
        connected_notify,
        sim,
        label,
        identity,
        dial_timeout,
    } = config;
    let policy = ReconnectPolicy::from_conn(&conn, sim);
    // The reconnect-jitter RNG is seeded from the configured havoc seed when one
    // is set, so jitter is reproducible (D.6). Both construction sites -
    // `client/data.rs` and `client/exec.rs` - pass
    // `seed: inbound_havoc.seed` into `WsConnectionConfig`, so a configured
    // seed reaches here; absent a seed we fall back to entropy.
    let mut rng = seed.map_or_else(|| StdRng::from_rng(&mut rand::rng()), StdRng::seed_from_u64);
    let mut attempt = 0u32;
    // `RunComplete` is a planned terminal state, not a transport failure. A
    // graceful close naming a terminal reason is its socket-level fallback for
    // a reader that loses the final text frame while the venue drains.
    //
    // The close code is not the signal - the reason is. This used to read any WS 1000 as completion,
    // which permanently disabled reconnect on every graceful close there is -
    // the venue's own eviction close is 1000, and so is a proxy retiring an
    // idle socket. `mogwai_protocol::close::classify` reads the reason string
    // the venue writes; anything it does not recognize falls through to the
    // ordinary disconnect-and-redial path, which is the safe default because a
    // needless redial is recoverable and a silently-ended run is not.
    let mut terminal: Option<mogwai_protocol::close::Terminal> = None;

    loop {
        if policy.exhausted(attempt) {
            // Only reachable with `reconnect_max_attempts = 0` (the true-return
            // from `backoff_or_exhausted` already logged and returned for every
            // other exhaustion), but a socket that never dials still deserves
            // its own `error` line.
            tracing::error!(
                socket = label,
                attempts = attempt,
                "reconnect attempts exhausted; giving up on the venue websocket"
            );
            connected.store(false, Ordering::Relaxed);
            return;
        }

        let ws = match tokio::time::timeout(dial_timeout, connect_async(&ws_url)).await {
            Ok(Ok((ws, _))) => ws,
            outcome => {
                // A cadence conflict is a failed dial like any other and stays
                // on the retry ladder. It gets its own line because the cause
                // is nothing like a transport outage and the operator would
                // otherwise read a silent backoff loop as one.
                if let Ok(Err(error)) = &outcome
                    && let Some(reason) = cadence_conflict_refusal(error)
                {
                    tracing::warn!(
                        socket = label,
                        url = %ws_url,
                        attempt,
                        %reason,
                        "venue refused this socket's cadence; another passenger of this account \
                         is riding this river at a different speed. Retrying: the refusal lifts \
                         when that passenger leaves. If it never lifts, the two legs of this \
                         client are configured with different speeds"
                    );
                    connected.store(false, Ordering::Relaxed);
                    if backoff_or_exhausted(&policy, &mut attempt, &mut rng, label).await {
                        return;
                    }
                    continue;
                }
                let error = match outcome {
                    Ok(Err(error)) => error.to_string(),
                    // The upgrade did not complete inside the deadline. On a cold
                    // river that usually means the venue is still synthesizing the
                    // warmup this dial asked for, so it is reported as the wait it
                    // is rather than folded into transport errors.
                    _ => format!(
                        "no upgrade within {dial_timeout:?}; the venue may still be materializing \
                         this river"
                    ),
                    // (the Ok(Ok(..)) arm is taken above)
                };
                tracing::warn!(
                    socket = label,
                    url = %ws_url,
                    attempt,
                    %error,
                    "venue websocket dial failed"
                );
                connected.store(false, Ordering::Relaxed);
                if backoff_or_exhausted(&policy, &mut attempt, &mut rng, label).await {
                    return;
                }
                continue;
            }
        };
        // Prove the thing that answered is this client's run, before it is used
        // for anything. An address identifies nothing over time: the port is
        // ephemeral, the venue frees it before it exits, and a client that only
        // knows where to dial cannot tell its own run from whatever answers
        // there next.
        //
        // A mismatch is always terminal and never retried. Reconnecting is for a venue
        // that went away and came back; a different run at the same address did
        // not come back, and dialling it again would only find it again.
        if let Some(check) = identity.as_ref()
            && let Err(reason) = verify_run_identity(check.as_ref(), label).await
        {
            tracing::error!(
                socket = label,
                url = %ws_url,
                %reason,
                "venue identity mismatch; this address is serving a different run, so this \
                 client is giving up rather than trading against it"
            );
            connected.store(false, Ordering::Relaxed);
            return;
        }

        // `attempt` counts dials since the last proven connection, so a nonzero
        // value here marks this line as a re-attach after an outage rather than
        // the boot-time connect.
        tracing::info!(socket = label, url = %ws_url, attempt, "venue websocket connected");

        // A successful dial alone does not, on its own, reset the attempt counter: a venue
        // that accepts the handshake and immediately dies would otherwise
        // restart the count on every cycle, so `reconnect_max_attempts` could
        // never trip and each re-dial would re-enter at the cheap initial
        // backoff. The counter resets only once the connection proves itself
        // by delivering an inbound application frame (Text/Binary, in the
        // select loop below) - the same liveness criterion the idle timeout
        // uses - so unproven connect/teardown cycles keep walking the
        // exponential ladder toward the cap.
        connected.store(true, Ordering::Relaxed);
        connected_notify.notify_waiters();
        let (mut writer, mut reader) = ws.split();
        let (out_tx, mut out_rx) = unbounded_channel::<(u64, Message)>();
        // The receipt book for commands accepted onto `out_tx` and not yet on
        // the socket. An entry is created before the frame is queued and
        // retired only by the same expression that observed `writer.send` come
        // back `Ok` - the frontier rule, applied to a write rather than to a
        // watermark. Whatever survives the writer's abort is exactly the set of
        // commands the venue never saw, and it is reported below.
        let unwritten: Arc<StdMutex<VecDeque<(u64, Cmd)>>> =
            Arc::new(StdMutex::new(VecDeque::new()));
        let mut next_seq: u64 = 0;
        let writer_unwritten = Arc::clone(&unwritten);
        let writer_handle = AbortOnDrop::new(tokio::spawn(async move {
            while let Some((seq, msg)) = out_rx.recv().await {
                if writer.send(msg).await.is_err() {
                    break;
                }
                // Nothing may await between the `Ok` above and this retire.
                // The retire is what makes the receipt book mean "the venue
                // never saw this"; an `await` inserted here would be a point
                // where the task can be aborted with the frame on the wire and
                // the receipt still standing, which reports a delivered command
                // as undelivered. A future edit that needs work here must do it
                // after the retire, not before.
                lock_unwritten(&writer_unwritten).retain(|(queued, _)| *queued != seq);
                // One spurious-reject window remains and is unavoidable: an
                // abort landing inside `send` after the bytes reached the wire
                // but before it returns leaves the receipt in place, so the
                // command is reported undelivered though the venue saw it. That
                // is the safe direction - the caller is told less happened than
                // did, never more - and the alternative (retiring before the
                // send) loses commands silently. It is stated here because it
                // is invisible at the reporting site.
            }
        }));
        let (in_tx, mut in_rx) = unbounded_channel();
        let reader_handle = AbortOnDrop::new(tokio::spawn(async move {
            while let Some(msg) = reader.next().await {
                if in_tx.send(Some(msg)).is_err() {
                    return;
                }
            }
            drop(in_tx.send(None));
        }));

        // Reattach state is WS commands only - resubscribes on the data socket,
        // nothing on the exec socket. In particular the execution client does
        // NOT re-pull `GET /account` here, so an `AccountState` the venue
        // dropped during a blackout that spanned this reconnect stays missing
        // from the pushed account row until the next fill-driven snapshot.
        //
        // That is deliberate, not an oversight. A real venue leaves you holding
        // a stale account view after an outage, and mogwai ships a divergence,
        // `DropNextAccountUpdate`, whose entire purpose is to inject exactly
        // this staleness - healing it automatically on every reconnect would
        // quietly undo the fault the operator armed. The path that must not be
        // fooled is already immune: `generate_position_status_reports` pulls
        // `GET /account` fresh on every call, so reconciliation reads venue
        // truth regardless of how stale the pushed row has become.
        send_reattach_commands(
            on_connect(),
            &out_tx,
            &serialize,
            &mut next_seq,
            &unwritten,
            on_undelivered,
        );

        let mut heartbeat = (conn.heartbeat_interval_ms > 0).then(|| {
            // `interval` starts ready, so its first `tick()` resolves
            // immediately. Anchoring the period one interval out keeps the
            // first Ping from firing right after connect (D.8): the cadence
            // should be one `heartbeat_interval_ms` after the socket comes up,
            // not at t=0.
            let period = sim.wall_duration(conn.heartbeat_interval_ms.saturating_mul(1_000_000));
            let mut interval = tokio::time::interval_at(Instant::now() + period, period);
            interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
            interval
        });
        let mut idle_sleep = idle_sleep(conn.idle_timeout_ms, sim);
        let mut commands_closed = false;

        loop {
            let action = tokio::select! {
                msg = in_rx.recv() => WsAction::Inbound(msg.flatten()),
                cmd = recv_command(cmd_rx), if !commands_closed => WsAction::Command(cmd),
                () = heartbeat_tick(&mut heartbeat), if heartbeat.is_some() => WsAction::Heartbeat,
                () = idle_tick(&mut idle_sleep), if idle_sleep.is_some() => WsAction::Idle,
            };

            match action {
                WsAction::Inbound(inbound) => {
                    if let Some(Ok(Message::Close(Some(frame)))) = &inbound
                        && let Some(kind) =
                            mogwai_protocol::close::classify(frame.code.into(), &frame.reason)
                    {
                        terminal = Some(kind);
                        break;
                    }
                    if let Some(cause) = disconnect_cause(&inbound) {
                        tracing::warn!(
                            socket = label,
                            %cause,
                            "venue websocket disconnected"
                        );
                        break;
                    }
                    match inbound.expect("inbound close and errors returned above") {
                        Ok(Message::Text(text)) => {
                            // Connection proven by an inbound application
                            // frame: reset the reconnect attempt counter (see
                            // the comment at the top of the connection).
                            attempt = 0;
                            reset_idle(&mut idle_sleep, conn.idle_timeout_ms, sim);
                            // Surface deserialization failures: a version-skewed
                            // or malformed frame would otherwise be swallowed
                            // while the idle clock was just reset, so the
                            // connection looks healthy while data silently drops
                            // (D.5).
                            match VenueMessage::from_json_str(&text) {
                                Ok(venue_msg) => {
                                    terminal = terminal.or_else(|| terminal_for(&venue_msg));
                                    handler(venue_msg).await;
                                    if terminal.is_some() {
                                        break;
                                    }
                                }
                                Err(error) => tracing::warn!(
                                    %error,
                                    "dropping unparseable text venue frame"
                                ),
                            }
                        }
                        Ok(Message::Binary(bytes)) => {
                            // Connection proven; same as the Text arm above.
                            attempt = 0;
                            reset_idle(&mut idle_sleep, conn.idle_timeout_ms, sim);
                            match VenueMessage::from_json_slice(&bytes) {
                                Ok(venue_msg) => {
                                    terminal = terminal.or_else(|| terminal_for(&venue_msg));
                                    handler(venue_msg).await;
                                    if terminal.is_some() {
                                        break;
                                    }
                                }
                                Err(error) => tracing::warn!(
                                    %error,
                                    "dropping unparseable binary venue frame"
                                ),
                            }
                        }
                        Ok(Message::Ping(payload)) => {
                            // Seq 0 on every non-command frame: pings and pongs
                            // carry no caller to report an undelivery to, and 0
                            // is never issued to a command, so a retire that
                            // matched it could not remove a receipt.
                            if out_tx.send((0, Message::Pong(payload))).is_err() {
                                tracing::warn!(
                                    socket = label,
                                    "websocket writer is gone; dropping the connection to re-dial"
                                );
                                break;
                            }
                        }
                        Ok(_) => {}
                        Err(_) => unreachable!("inbound errors returned above"),
                    }
                }
                WsAction::Command(Some(cmd)) => {
                    next_seq += 1;
                    if let Some(cmd) = send_command(&out_tx, &serialize, cmd, next_seq, &unwritten)
                    {
                        on_undelivered(cmd);
                        tracing::warn!(
                            socket = label,
                            "websocket writer is gone; dropping the connection to re-dial"
                        );
                        break;
                    }
                }
                WsAction::Command(None) => {
                    commands_closed = true;
                }
                WsAction::Heartbeat => {
                    if out_tx.send((0, Message::Ping(Vec::new().into()))).is_err() {
                        tracing::warn!(
                            socket = label,
                            "websocket writer is gone; dropping the connection to re-dial"
                        );
                        break;
                    }
                }
                WsAction::Idle => {
                    tracing::warn!(
                        socket = label,
                        idle_timeout_ms = conn.idle_timeout_ms,
                        "no inbound frame within the idle timeout; dropping the connection to re-dial"
                    );
                    break;
                }
            }
        }

        // Abort then join: aborting only requests cancellation, so awaiting the
        // handles makes the writer/reader tasks observably quiesced before the
        // next reconnect iteration spins up a fresh socket and a new pair of
        // tasks (D.12). Without the join an in-flight writer send could still be
        // racing the teardown. A `JoinError` here is expected - the task was
        // just aborted - so it is intentionally ignored.
        writer_handle.abort_and_join().await;
        reader_handle.abort_and_join().await;
        // The writer is now observably dead, so the receipt book cannot move
        // again and what it holds is final: every command accepted onto this
        // socket and never written. Report each, oldest first, then drop the
        // sender so nothing can file a receipt against a socket that is gone.
        drop(out_tx);
        let residue: Vec<Cmd> = lock_unwritten(&unwritten)
            .drain(..)
            .map(|(_, c)| c)
            .collect();
        if !residue.is_empty() {
            tracing::warn!(
                socket = label,
                commands = residue.len(),
                "the venue websocket dropped with commands still queued; reporting them \
                 undelivered rather than replaying them onto the next connection"
            );
        }
        for cmd in residue {
            on_undelivered(cmd);
        }
        on_disconnect().await;
        connected.store(false, Ordering::Relaxed);
        if let Some(kind) = terminal {
            // Each of these is terminal, and saying which one matters to an
            // operator reading the log: a completed run is the expected end of
            // a connection, while an eviction means another connection claimed
            // this account under a different callsign - a configuration
            // problem, not a finish line.
            //
            // All three arms are now exact. The venue announces a finished run
            // and an elapsed passenger duration as different frames, so the
            // inbound-text arm above classifies each correctly and breaks
            // without needing the close that follows; the close agrees with it
            // and remains the fallback for a reader that lost the frame.
            // Eviction has no text frame at all and was always exact. Until the
            // frames split, a duration end was logged here as a finished run.
            match kind {
                mogwai_protocol::close::Terminal::RunComplete => {
                    tracing::info!(socket = label, "venue run completed; reconnect disabled");
                }
                mogwai_protocol::close::Terminal::DurationComplete => {
                    tracing::info!(
                        socket = label,
                        "this connection's configured duration elapsed; reconnect disabled"
                    );
                }
                mogwai_protocol::close::Terminal::Evicted => {
                    tracing::warn!(
                        socket = label,
                        "another connection claimed this account and the venue evicted this \
                         socket; reconnect disabled, because redialling would evict the claimant \
                         in turn"
                    );
                }
            }
            return;
        }
        // An established connection dropping (peer Close, read error, idle
        // timeout) re-enters the dial through the same backoff as a failed
        // dial. The sleep used to live only on the `connect_async` Err arm, so
        // an accept-then-die venue produced an unthrottled connect/teardown
        // flood with zero delay between cycles - and because dialing kept
        // succeeding (and used to reset the counter), the attempt cap never
        // tripped. Sleeping and escalating here restores production reconnect
        // semantics: a proven connection re-dials after the initial backoff
        // (the counter was reset by its first inbound frame), while unproven
        // cycles compound toward `reconnect_max_attempts`. The delay is
        // sim-clock-scaled inside `backoff`, like every other lifecycle sleep.
        // If advancing the counter here spends the last permitted attempt, the
        // loop returns immediately rather than sleeping a final backoff before
        // the loop-top exhausted check would return anyway (F12).
        if backoff_or_exhausted(&policy, &mut attempt, &mut rng, label).await {
            return;
        }
    }
}

/// Recognize the venue's second-cadence refusal in a failed upgrade, and return
/// its text.
///
/// This classifies for logging only. The refusal is conditional, not permanent:
/// `docs/accounts.md` states the rule holds while any of the account's
/// passengers is riding that river and lifts once the last leaves. The
/// conflicting passenger need not belong to this client at all - any process
/// naming the same account id may hold that river - and even one that does can
/// reach its own `duration_ms` or be stopped by its host. So the admissible
/// window opens without anything happening on this side, and a client that
/// disabled reconnect here would sit out a run it could have joined.
///
/// The terminal reading was tried and rejected for that reason. It is not
/// merely unproven in general: it is unprovable from here even in the case that
/// looks provable, where the conflicting cadence is held by this client's other
/// leg. The venue names the seated speed but not who is seated at it, the two
/// legs are separate client objects with no handle on each other, and a leg's
/// own passenger duration can end at any time. There is no expression available
/// in this task that distinguishes a conflict which will clear from one which
/// will not, so every conflict is retried and the log line names the
/// misconfiguration that is the likely cause when it never clears.
fn cadence_conflict_refusal(error: &tokio_tungstenite::tungstenite::Error) -> Option<String> {
    let tokio_tungstenite::tungstenite::Error::Http(response) = error else {
        return None;
    };
    if response.status() != tokio_tungstenite::tungstenite::http::StatusCode::BAD_REQUEST {
        return None;
    }
    let body = response.body().as_deref()?;
    let reason = std::str::from_utf8(body).ok()?;
    reason
        .contains(mogwai_protocol::control::CADENCE_CONFLICT_MARKER)
        .then(|| reason.to_owned())
}

async fn recv_command<Cmd>(rx: &mut Option<UnboundedReceiver<Cmd>>) -> Option<Cmd> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// Classifies an inbound reader item: `None` for a live frame the select loop
/// should process, otherwise the operator-facing cause of the disconnect. The
/// cause matters because a venue outage-and-recovery used to leave zero
/// connection-lifecycle lines in the log - the reconnect machinery worked
/// silently while an unrelated `warn` storm named phantom causes - so the one
/// line announcing the disconnect must say what actually happened on the
/// socket.
///
/// The close code is reported, never acted on in this function - the one close this loop
/// acts on is classified upstream by `mogwai_protocol::close::classify`, from
/// the close reason, and never reaches this function. A venue fault closes the
/// socket with WS 1011, and that is deliberately treated here as an ordinary
/// disconnect: the adapter reconnects, resubscribes, and carries on. Making a
/// venue fault terminal end to end is the consumer's decision, not the
/// adapter's - mogwai's job is to state the fault as clearly as it can and
/// decline to repair it downstream, the same principle that governs reconnect
/// account staleness above. An adapter that unilaterally killed the run would be
/// making a policy call on behalf of whoever embedded it.
///
/// A lagged feed is not one of these, though it used to be. The venue declares a
/// market-view hole with `FeedLagged` and keeps serving, so no disconnect
/// happens at all and this function never sees it.
/// Which terminal a venue frame announces, if any.
///
/// The two terminal announcements are distinct variants precisely so this can
/// be exact. It used to test for `RunComplete` alone, because that was the only
/// frame either completion sent - so a passenger whose own duration elapsed was
/// logged, and reported, as a finished run. The close reason carried the truth
/// and was read only when the frame was missed.
///
/// The close remains the socket-level fallback for a reader that loses the final
/// text frame while the venue drains; it now agrees with the frame rather than
/// refining it.
fn terminal_for(message: &VenueMessage) -> Option<mogwai_protocol::close::Terminal> {
    match message {
        VenueMessage::RunComplete { .. } => Some(mogwai_protocol::close::Terminal::RunComplete),
        VenueMessage::PassengerDurationComplete { .. } => {
            Some(mogwai_protocol::close::Terminal::DurationComplete)
        }
        _ => None,
    }
}

fn disconnect_cause(
    inbound: &Option<Result<Message, tokio_tungstenite::tungstenite::Error>>,
) -> Option<String> {
    match inbound {
        None => Some("stream ended without a close frame".to_string()),
        Some(Err(error)) => Some(format!("read error: {error}")),
        Some(Ok(Message::Close(frame))) => Some(match frame {
            Some(frame) => format!(
                "peer sent close (code {:?}, reason {:?})",
                frame.code, frame.reason
            ),
            None => "peer sent close".to_string(),
        }),
        Some(Ok(_)) => None,
    }
}

/// Queues the reattach commands a fresh connection owes the venue, reporting
/// every single one it could not queue - including the ones behind the failure.
///
/// Telling the caller is never optional, and that applies to the untried
/// remainder too. This used to `break` after reporting the single command that
/// hit a dead writer, dropping the rest of the vector on the floor: the caller
/// heard about one loss and nothing about the ones after it, which is the same
/// silent-loss shape the receipt book exists to close. The receipt book cannot
/// cover these, because a command that was never queued never got a receipt
/// filed for it, so the remainder is reported here or nowhere.
///
/// Stopping at the first failure is still right - the writer is gone and every
/// later `send_command` would fail identically - so the loop stops trying and
/// keeps reporting. `seq` is advanced by reference so the caller's numbering
/// continues past whatever was queued here.
fn send_reattach_commands<Cmd, Serialize, OnUndelivered>(
    commands: Vec<Cmd>,
    out_tx: &UnboundedSender<(u64, Message)>,
    serialize: &Serialize,
    seq: &mut u64,
    unwritten: &Arc<StdMutex<VecDeque<(u64, Cmd)>>>,
    on_undelivered: &mut OnUndelivered,
) where
    Serialize: Fn(&Cmd) -> Command,
    OnUndelivered: FnMut(Cmd),
{
    let mut pending = commands.into_iter();
    while let Some(cmd) = pending.next() {
        *seq += 1;
        if let Some(cmd) = send_command(out_tx, serialize, cmd, *seq, unwritten) {
            on_undelivered(cmd);
            for remaining in pending {
                on_undelivered(remaining);
            }
            return;
        }
    }
}

/// Encodes `cmd` and queues it for the writer, recording a receipt in
/// `unwritten` so a socket that dies before the frame is written can say which
/// commands it swallowed. See `run_ws_connection`'s `on_undelivered`.
///
/// Returns the command back on failure rather than dropping it: an encode
/// failure and a dead writer channel are both undeliveries the caller owes a
/// report on, and handing the value back is what lets it make one. A `None`
/// return means the command is now the receipt book's problem.
fn send_command<Cmd, Serialize>(
    out_tx: &UnboundedSender<(u64, Message)>,
    serialize: &Serialize,
    cmd: Cmd,
    seq: u64,
    unwritten: &Arc<StdMutex<VecDeque<(u64, Cmd)>>>,
) -> Option<Cmd>
where
    Serialize: Fn(&Cmd) -> Command,
{
    let msg = serialize(&cmd);
    let Ok(payload) = serde_json::to_string(&msg) else {
        tracing::error!("dropping a websocket command that would not encode");
        return Some(cmd);
    };
    // The receipt goes in before the queue, never after: the writer can retire
    // a seq the instant the frame is queued, and a receipt filed afterwards
    // would then outlive the write it was meant to cover and be reported as
    // undelivered when the socket dies.
    lock_unwritten(unwritten).push_back((seq, cmd));
    if out_tx.send((seq, Message::Text(payload.into()))).is_err() {
        let mut book = lock_unwritten(unwritten);
        if let Some(at) = book.iter().position(|(queued, _)| *queued == seq) {
            return book.remove(at).map(|(_, cmd)| cmd);
        }
    }
    None
}

/// The receipt book's lock, recovered on poison. A panicked writer must not
/// turn every later undelivery into a silent one, which is what propagating
/// the poison would do.
fn lock_unwritten<Cmd>(
    unwritten: &Arc<StdMutex<VecDeque<(u64, Cmd)>>>,
) -> std::sync::MutexGuard<'_, VecDeque<(u64, Cmd)>> {
    unwritten
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn idle_sleep(timeout_ms: u64, sim: SimClock) -> Option<Pin<Box<Sleep>>> {
    (timeout_ms > 0).then(|| {
        Box::pin(tokio::time::sleep(
            sim.wall_duration(timeout_ms.saturating_mul(1_000_000)),
        ))
    })
}

fn reset_idle(idle_sleep: &mut Option<Pin<Box<Sleep>>>, timeout_ms: u64, sim: SimClock) {
    if let Some(sleep) = idle_sleep {
        sleep
            .as_mut()
            .reset(Instant::now() + sim.wall_duration(timeout_ms.saturating_mul(1_000_000)));
    }
}

async fn heartbeat_tick(heartbeat: &mut Option<tokio::time::Interval>) {
    if let Some(heartbeat) = heartbeat {
        heartbeat.tick().await;
    }
}

async fn idle_tick(idle_sleep: &mut Option<Pin<Box<Sleep>>>) {
    if let Some(sleep) = idle_sleep {
        sleep.as_mut().await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    /// The classifier alone, on hand-built responses. It says only that the
    /// marker is read out of a 400 body and that a neighbouring 400 is not
    /// mistaken for one. What the connection does with that answer is a
    /// lifecycle question and is covered over a real socket by
    /// `a_cadence_conflict_is_retried_until_the_incumbent_leaves` in
    /// `tests/havoc.rs`; this test cannot see it.
    #[test]
    fn a_cadence_conflict_is_read_out_of_the_venues_400_body() {
        let response = tokio_tungstenite::tungstenite::http::Response::builder()
            .status(tokio_tungstenite::tungstenite::http::StatusCode::BAD_REQUEST)
            .body(Some(
                format!(
                    "account A is already seated on MNQ at speed 1; {}",
                    mogwai_protocol::control::CADENCE_CONFLICT_MARKER
                )
                .into_bytes(),
            ))
            .unwrap();
        let error = tokio_tungstenite::tungstenite::Error::Http(Box::new(response));
        assert!(cadence_conflict_refusal(&error).is_some());

        let other = tokio_tungstenite::tungstenite::http::Response::builder()
            .status(tokio_tungstenite::tungstenite::http::StatusCode::BAD_REQUEST)
            .body(Some(b"speed must be finite and non-negative".to_vec()))
            .unwrap();
        assert!(
            cadence_conflict_refusal(&tokio_tungstenite::tungstenite::Error::Http(Box::new(
                other
            )))
            .is_none(),
            "an unrelated 400 must not be reported as a cadence conflict"
        );
    }

    fn a_cancel(id: &str) -> Command {
        Command::CancelOrder {
            client_order_id: id.to_string(),
        }
    }

    /// `Command` is not `PartialEq`, and its wire form is the identity
    /// that matters here anyway: these tests assert which command came back.
    fn ids_of(msgs: &[Command]) -> Vec<String> {
        msgs.iter()
            .map(|msg| match msg {
                Command::CancelOrder { client_order_id } => client_order_id.clone(),
                other => panic!("unexpected command {other:?}"),
            })
            .collect()
    }

    /// The receipt book, at the level where its two halves are separable.
    ///
    /// `send_command` files a receipt before queueing the frame, and only the
    /// expression that saw `writer.send` return `Ok` retires it - so a frame
    /// still sitting in the writer's queue when the socket dies is still in the
    /// book, which is exactly the set `run_ws_connection` reports undelivered.
    /// Without the book there is nothing to report: the channel `send` returned
    /// `Ok` and the command was consumed.
    #[test]
    fn a_queued_frame_leaves_a_receipt_until_the_writer_retires_it() {
        let (out_tx, mut out_rx) = unbounded_channel::<(u64, Message)>();
        let unwritten: Arc<StdMutex<VecDeque<(u64, Command)>>> =
            Arc::new(StdMutex::new(VecDeque::new()));
        let serialize = |cmd: &Command| cmd.clone();

        assert!(
            send_command(&out_tx, &serialize, a_cancel("A"), 1, &unwritten).is_none(),
            "a live writer channel accepts the command"
        );
        assert!(
            send_command(&out_tx, &serialize, a_cancel("B"), 2, &unwritten).is_none(),
            "a live writer channel accepts the second command too"
        );
        assert_eq!(
            lock_unwritten(&unwritten).len(),
            2,
            "both commands are queued and unwritten, so both hold receipts"
        );

        // The writer drains and writes only the first frame, then the socket
        // dies - the abort case, without the abort.
        let (seq, _) = out_rx.try_recv().expect("the first frame is queued");
        lock_unwritten(&unwritten).retain(|(queued, _)| *queued != seq);

        let residue: Vec<Command> = lock_unwritten(&unwritten)
            .drain(..)
            .map(|(_, cmd)| cmd)
            .collect();
        assert_eq!(
            ids_of(&residue),
            vec!["B".to_string()],
            "only the command the writer never wrote is left to report"
        );
    }

    /// A dead writer channel hands the command straight back, so the caller can
    /// report it rather than believing it was queued.
    #[test]
    fn a_command_queued_onto_a_dead_writer_comes_straight_back() {
        let (out_tx, out_rx) = unbounded_channel::<(u64, Message)>();
        drop(out_rx);
        let unwritten: Arc<StdMutex<VecDeque<(u64, Command)>>> =
            Arc::new(StdMutex::new(VecDeque::new()));
        let returned = send_command(
            &out_tx,
            &|cmd: &Command| cmd.clone(),
            a_cancel("A"),
            1,
            &unwritten,
        );
        assert_eq!(
            ids_of(&returned.into_iter().collect::<Vec<_>>()),
            vec!["A".to_string()],
            "a dead writer channel hands the command back rather than swallowing it"
        );
        assert!(
            lock_unwritten(&unwritten).is_empty(),
            "a command handed back is the caller's again, not the book's - reporting it \
             from both places would double-reject the order"
        );
    }

    /// The commands behind the failure are undeliveries too.
    ///
    /// A dead writer fails the first reattach command, and the rest of the
    /// vector is never even tried - so no receipt exists for any of them and
    /// the caller hears about them here or never. Reporting only the one that
    /// failed is the shape this test exists to forbid: it is indistinguishable,
    /// from the caller's side, from the others having been delivered.
    ///
    /// Both shipped clients pass `Vec::new` today, so this is the only place
    /// the contract is exercised at all.
    #[test]
    fn every_untried_reattach_command_is_reported_not_just_the_one_that_failed() {
        let (out_tx, out_rx) = unbounded_channel::<(u64, Message)>();
        drop(out_rx);
        let unwritten: Arc<StdMutex<VecDeque<(u64, Command)>>> =
            Arc::new(StdMutex::new(VecDeque::new()));
        let mut reported: Vec<Command> = Vec::new();
        let mut seq = 7;
        send_reattach_commands(
            vec![a_cancel("A"), a_cancel("B"), a_cancel("C")],
            &out_tx,
            &|cmd: &Command| cmd.clone(),
            &mut seq,
            &unwritten,
            &mut |cmd| reported.push(cmd),
        );
        assert_eq!(
            ids_of(&reported),
            vec!["A".to_string(), "B".to_string(), "C".to_string()],
            "the untried remainder was dropped instead of reported"
        );
        assert!(
            lock_unwritten(&unwritten).is_empty(),
            "a command handed back is the caller's again, so no receipt may survive - \
             reporting it twice would double-reject the order"
        );
        assert_eq!(
            seq, 8,
            "only the command that was actually attempted consumed a sequence number"
        );
    }

    /// The loop's exit is itself an undelivery. Commands still sitting in the command
    /// receiver when `run_ws_connection` returns die with the receiver, and
    /// nothing else in this crate can see them: `send_command` never ran, so no
    /// receipt exists, and the caller was told `Ok` when it enqueued.
    ///
    /// `reconnect_max_attempts = 0` is what makes this deterministic rather
    /// than a race with the select loop - the loop returns at its top without
    /// ever dialing, so every queued command is guaranteed unread.
    #[tokio::test(flavor = "current_thread")]
    async fn commands_queued_when_the_loop_gives_up_are_reported_undelivered() {
        let (cmd_tx, cmd_rx) = unbounded_channel::<Command>();
        cmd_tx.send(a_cancel("A")).expect("queue A");
        cmd_tx.send(a_cancel("B")).expect("queue B");
        let conn = ConnHavoc {
            reconnect_max_attempts: Some(0),
            ..Default::default()
        };
        // Port 0 is never dialed: the attempt cap is already spent at the loop
        // top, so this test binds no socket at all.
        let undelivered = run_lifecycle_recording(0, conn, cmd_rx).await;
        let reported = undelivered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        assert_eq!(
            ids_of(&reported),
            vec!["A".to_string(), "B".to_string()],
            "every queued command is reported once, oldest first"
        );
    }

    /// A venue that answers, correctly, and reports no run is version skew - not
    /// a transport failure, and not a mismatch.
    ///
    /// All three non-mismatch paths behave identically (the connection
    /// proceeds), so no assertion on behaviour can tell them apart; only the
    /// category distinguishes them, and only this test pins it. The skew case
    /// was filed under `Unreachable` once, which put a correct detail under a
    /// headline contradicting it and hid a real version mismatch from anyone
    /// grepping the category it was filed as.
    #[test]
    fn a_venue_that_reports_no_run_is_skew_not_a_transport_failure() {
        let skew = classify_identity(Err(format!(
            "{IDENTITY_NOT_REPORTED}http://x/health answered without a run_seed"
        )));
        assert!(
            matches!(skew, IdentityOutcome::Unidentified(_)),
            "an answered probe with no run is skew, got {skew:?}"
        );

        for unreachable in [
            format!("{IDENTITY_UNREACHABLE}http://x/health: connection refused"),
            format!("{IDENTITY_UNREACHABLE}http://x/health answered 500"),
            format!("{IDENTITY_UNREACHABLE}http://x/health is not JSON: eof"),
        ] {
            let outcome = classify_identity(Err(unreachable.clone()));
            assert!(
                matches!(outcome, IdentityOutcome::Unreachable(_)),
                "{unreachable} is no usable answer, got {outcome:?}"
            );
        }

        // Anything not carrying a category prefix is the venue answering as
        // someone else, which is the only outcome that refuses.
        assert!(matches!(
            classify_identity(Err(
                "expected run 7, but this address is serving run 42".to_owned()
            )),
            IdentityOutcome::Mismatch(_)
        ));
        assert_eq!(classify_identity(Ok(())), IdentityOutcome::Confirmed);
    }

    #[test]
    fn reconnect_policy_backoff_grows_and_clamps() {
        let conn = ConnHavoc {
            reconnect_delay_initial_ms: 100,
            reconnect_delay_max_ms: 1_000,
            reconnect_backoff_factor: 2.0,
            ..Default::default()
        };
        let policy = ReconnectPolicy::from_conn(&conn, SimClock::identity());
        let mut rng = StdRng::seed_from_u64(7);

        assert_eq!(policy.backoff(0, &mut rng), Duration::from_millis(100));
        assert_eq!(policy.backoff(1, &mut rng), Duration::from_millis(200));
        assert_eq!(policy.backoff(4, &mut rng), Duration::from_millis(1_000));
    }

    #[test]
    fn reconnect_policy_jitter_is_seeded() {
        let conn = ConnHavoc {
            reconnect_delay_initial_ms: 100,
            reconnect_delay_max_ms: 100,
            reconnect_jitter_ms: 50,
            ..Default::default()
        };
        let policy = ReconnectPolicy::from_conn(&conn, SimClock::identity());
        let mut left = StdRng::seed_from_u64(11);
        let mut right = StdRng::seed_from_u64(11);

        let left_draws = [
            policy.backoff(0, &mut left),
            policy.backoff(0, &mut left),
            policy.backoff(0, &mut left),
        ];
        let right_draws = [
            policy.backoff(0, &mut right),
            policy.backoff(0, &mut right),
            policy.backoff(0, &mut right),
        ];

        assert_eq!(left_draws, right_draws);
        assert!(
            left_draws
                .iter()
                .all(|d| { *d >= Duration::from_millis(100) && *d <= Duration::from_millis(150) })
        );
    }

    #[test]
    fn reconnect_policy_zero_max_does_not_clamp_to_zero() {
        // D.3: a nonzero initial paired with a zero ceiling used to collapse to
        // a zero delay (a busy-spin). The protocol validator rejects this
        // config outright; this test pins the belt-and-suspenders guard for a
        // caller that bypasses it - `max == 0` means "no clamp", so the
        // exponential base grows from the configured initial instead.
        let conn = ConnHavoc {
            reconnect_delay_initial_ms: 100,
            reconnect_delay_max_ms: 0,
            reconnect_backoff_factor: 2.0,
            ..Default::default()
        };
        let policy = ReconnectPolicy::from_conn(&conn, SimClock::identity());
        let mut rng = StdRng::seed_from_u64(3);

        assert_eq!(policy.backoff(0, &mut rng), Duration::from_millis(100));
        assert_eq!(policy.backoff(1, &mut rng), Duration::from_millis(200));
        assert_eq!(policy.backoff(3, &mut rng), Duration::from_millis(800));
    }

    #[test]
    fn reconnect_policy_zero_initial_stays_zero() {
        // Both bounds zero is the one validated backoff-disabled configuration
        // (the protocol validator rejects either bound at zero while the other
        // is positive), and it yields a genuinely zero delay on every attempt.
        let conn = ConnHavoc {
            reconnect_delay_initial_ms: 0,
            reconnect_delay_max_ms: 0,
            reconnect_backoff_factor: 2.0,
            ..Default::default()
        };
        let policy = ReconnectPolicy::from_conn(&conn, SimClock::identity());
        let mut rng = StdRng::seed_from_u64(3);

        assert_eq!(policy.backoff(0, &mut rng), Duration::ZERO);
        assert_eq!(policy.backoff(5, &mut rng), Duration::ZERO);
    }

    #[test]
    fn http_quota_rate_three_rounds_spacing_up() {
        // D.9: 3 requests/sec does not divide one second evenly. Ceil-dividing
        // 1e9 / 3 yields 333_333_334ns (>= 333_333_333), so the spacing never
        // undershoots and the effective rate stays at or below the cap.
        let quota = HttpQuota::from_conn(
            &ConnHavoc {
                max_requests_per_second: Some(3),
                ..Default::default()
            },
            SimClock::identity(),
        );

        assert_eq!(quota.min_interval, Some(Duration::from_nanos(333_333_334)));
    }

    #[test]
    fn reconnect_policy_exhausted_flips_at_cap() {
        let conn = ConnHavoc {
            reconnect_max_attempts: Some(3),
            ..Default::default()
        };
        let policy = ReconnectPolicy::from_conn(&conn, SimClock::identity());

        assert!(!policy.exhausted(0));
        assert!(!policy.exhausted(2));
        assert!(policy.exhausted(3));
    }

    #[test]
    fn reconnect_policy_backoff_scales_with_sim_clock() {
        let conn = ConnHavoc {
            reconnect_delay_initial_ms: 100,
            reconnect_delay_max_ms: 1_000,
            reconnect_backoff_factor: 2.0,
            ..Default::default()
        };
        let policy = ReconnectPolicy::from_conn(
            &conn,
            SimClock {
                sim_epoch_ns: 1,
                wall_anchor_ns: 1,
                speed: 10.0,
            },
        );
        let mut rng = StdRng::seed_from_u64(7);

        assert_eq!(policy.backoff(0, &mut rng), Duration::from_millis(10));
        assert_eq!(policy.backoff(1, &mut rng), Duration::from_millis(20));
    }

    #[test]
    fn http_quota_interval_scales_with_sim_clock() {
        let quota = HttpQuota::from_conn(
            &ConnHavoc {
                max_requests_per_second: Some(10),
                ..Default::default()
            },
            SimClock {
                sim_epoch_ns: 1,
                wall_anchor_ns: 1,
                speed: 10.0,
            },
        );

        assert_eq!(quota.min_interval, Some(Duration::from_millis(10)));
    }

    #[test]
    fn conn_reconnect_policy_respects_max_attempts() {
        let conn = ConnHavoc {
            reconnect_max_attempts: Some(2),
            ..Default::default()
        };
        let policy = ReconnectPolicy::from_conn(&conn, SimClock::identity());

        assert!(!policy.exhausted(0));
        assert!(!policy.exhausted(1));
        assert!(policy.exhausted(2));
    }

    #[tokio::test]
    async fn conn_http_quota_spaces_requests() {
        let quota = HttpQuota::from_conn(
            &ConnHavoc {
                max_requests_per_second: Some(20),
                ..Default::default()
            },
            SimClock::identity(),
        );

        quota.wait().await;
        let first = Instant::now();
        quota.wait().await;
        let elapsed = first.elapsed();

        assert!(
            elapsed >= Duration::from_millis(50),
            "quota allowed second request after {elapsed:?}"
        );
    }

    /// Completes one venue-side WS handshake on `listener`.
    async fn accept_ws(
        listener: &tokio::net::TcpListener,
    ) -> tokio_tungstenite::WebSocketStream<tokio::net::TcpStream> {
        let (stream, _) = listener.accept().await.expect("accept ws dial");
        tokio_tungstenite::accept_async(stream)
            .await
            .expect("venue ws handshake")
    }

    /// Drives `run_ws_connection` with inert callbacks against a loopback stub.
    async fn run_lifecycle(port: u16, conn: ConnHavoc) {
        let (_cmd_tx, cmd_rx) = unbounded_channel::<Command>();
        drop(run_lifecycle_recording(port, conn, cmd_rx).await);
    }

    /// As `run_lifecycle`, but drives a caller-supplied command receiver and
    /// returns every command reported undelivered, in report order.
    async fn run_lifecycle_recording(
        port: u16,
        conn: ConnHavoc,
        cmd_rx: UnboundedReceiver<Command>,
    ) -> Arc<StdMutex<Vec<Command>>> {
        let undelivered = Arc::new(StdMutex::new(Vec::new()));
        let sink = Arc::clone(&undelivered);
        run_ws_connection(
            WsConnectionConfig {
                ws_url: format!("ws://127.0.0.1:{port}/ws"),
                conn,
                seed: Some(1),
                connected: Arc::new(AtomicBool::new(false)),
                connected_notify: Arc::new(tokio::sync::Notify::new()),
                sim: SimClock::identity(),
                label: "test",
                identity: None,
                dial_timeout: Duration::from_secs(mogwai_protocol::DEFAULT_DIAL_TIMEOUT_SECS),
            },
            Some(cmd_rx),
            |cmd: &Command| cmd.clone(),
            Vec::new,
            |_msg: VenueMessage| async {},
            || async {},
            move |cmd: Command| {
                sink.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(cmd);
            },
        )
        .await;
        undelivered
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "binds a real TCP listener; run in a socket-capable environment"]
    async fn reconnect_backoff_throttles_accept_then_die_and_trips_attempt_cap() {
        // A venue that completes the WS handshake and immediately closes must
        // not produce an unthrottled connect/teardown flood: dropping an
        // unproven connection sleeps the backoff and escalates the attempt
        // counter, so the cap eventually trips and the loop returns. Before
        // the fix the counter reset on every successful dial and the backoff
        // sleep lived only on the failed-dial arm, so this scenario redialed
        // forever with zero delay between cycles.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind stub listener");
        let port = listener.local_addr().expect("stub addr").port();
        let dials = Arc::new(AtomicUsize::new(0));
        let venue_dials = Arc::clone(&dials);
        let venue = tokio::spawn(async move {
            loop {
                let mut ws = accept_ws(&listener).await;
                venue_dials.fetch_add(1, Ordering::Relaxed);
                drop(ws.close(None).await);
            }
        });

        // The ladder escalates on purpose, and that is what buys this test its
        // margin. With a flat 100 ms backoff the two legal outcomes sit 100 ms
        // apart - two sleeps (200 ms) versus the pointless third (300 ms) - so
        // the upper bound had 100 ms to cover three loopback dials, three WS
        // handshakes and three task spawns, measured here at 3-4.5 ms on an idle
        // box but unbudgeted under an eight-way gate. Doubling instead makes the
        // first two sleeps 100 + 200 = 300 ms and a third 400 ms more, so the
        // window below is 250..500 against a defect that lands at ~700: ~200 ms
        // of headroom on each side for 100 ms of extra wall. The separation is
        // the signal here, not the assertion - widening the window would only have
        // moved the goalposts toward the defect.
        let conn = ConnHavoc {
            reconnect_delay_initial_ms: 100,
            reconnect_delay_max_ms: 1_000,
            reconnect_backoff_factor: 2.0,
            reconnect_max_attempts: Some(3),
            ..Default::default()
        };
        let started = Instant::now();
        tokio::time::timeout(Duration::from_secs(5), run_lifecycle(port, conn))
            .await
            .expect("the attempt cap must trip under accept-then-die");
        let elapsed = started.elapsed();

        assert_eq!(
            dials.load(Ordering::Relaxed),
            3,
            "a three-attempt cap must dial exactly three unproven connections"
        );
        assert!(
            (Duration::from_millis(250)..Duration::from_millis(500)).contains(&elapsed),
            "three dials span exactly two backoffs, 100ms then 200ms (~300ms): the \
             first two unproven drops sleep before redialing, but the third, \
             exhausting cycle returns immediately instead of sleeping a pointless \
             final backoff (F12) - a third sleep would add 400ms and push elapsed \
             to ~700ms, and skipping the sleeps altogether leaves it near zero \
             (elapsed {elapsed:?})"
        );
        venue.abort();
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "binds a real TCP listener; run in a socket-capable environment"]
    async fn run_complete_stops_the_reconnect_loop() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind stub listener");
        let port = listener.local_addr().expect("stub addr").port();
        let venue = tokio::spawn(async move {
            let mut ws = accept_ws(&listener).await;
            let frame = serde_json::to_string(&VenueMessage::RunComplete {
                sim_now_ns: 30,
                elapsed_ns: 30,
            })
            .expect("encode completion");
            ws.send(Message::Text(frame.into()))
                .await
                .expect("send completion");
            ws.close(None).await.expect("close completion socket");
        });

        tokio::time::timeout(
            Duration::from_secs(2),
            run_lifecycle(port, ConnHavoc::default()),
        )
        .await
        .expect("completion stops lifecycle without reconnecting");
        venue.await.expect("completion stub exits");
    }

    /// A graceful close whose reason this protocol does not define is a
    /// transport event, not a finish line: the loop must redial.
    ///
    /// This is the defect directly. The old arm read `CloseCode::Normal` alone,
    /// so any 1000 at all - a proxy retiring an idle socket, a venue restart, and the
    /// venue's own eviction close, which is deliberately 1000 - permanently
    /// disabled reconnect with an `info` line claiming the run had completed.
    /// The stub sends a 1000 carrying a reason this protocol does not
    /// define - a proxy's idle close is the everyday shape - and the attempt
    /// cap is what makes the test terminate: three dials means the loop kept
    /// redialling, one means it treated the close as terminal.
    ///
    /// The reason must be present. `ws.close(None)` sends `Close(None)`, which
    /// the old code did not match either (it required `Close(Some(frame))`), so
    /// a stub closing with `None` passes against the defect and proves nothing.
    /// A first draft of this test did exactly that and was caught by its own
    /// bite-check.
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "binds a real TCP listener; run in a socket-capable environment"]
    async fn an_unreasoned_normal_close_is_redialled_not_read_as_completion() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind stub listener");
        let port = listener.local_addr().expect("stub addr").port();
        let dials = Arc::new(AtomicUsize::new(0));
        let venue_dials = Arc::clone(&dials);
        let venue = tokio::spawn(async move {
            loop {
                let mut ws = accept_ws(&listener).await;
                venue_dials.fetch_add(1, Ordering::Relaxed);
                drop(
                    ws.close(Some(tokio_tungstenite::tungstenite::protocol::CloseFrame {
                        code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Normal,
                        reason: "idle connection retired".into(),
                    }))
                    .await,
                );
            }
        });
        let conn = ConnHavoc {
            reconnect_delay_initial_ms: 10,
            reconnect_delay_max_ms: 20,
            reconnect_backoff_factor: 1.0,
            reconnect_max_attempts: Some(3),
            ..Default::default()
        };
        tokio::time::timeout(Duration::from_secs(5), run_lifecycle(port, conn))
            .await
            .expect("the attempt cap must trip");
        assert_eq!(
            dials.load(Ordering::Relaxed),
            3,
            "an unreasoned 1000 close must redial to the cap; reading the code alone as \
             completion returns after the first close instead"
        );
        venue.abort();
    }

    /// The reason string is what makes a 1000 terminal, and the venue writes
    /// exactly `mogwai_protocol::close::RUN_COMPLETE`. This is the other half
    /// of the test above: the same close code, one recognized reason, and the
    /// loop stops without redialling.
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "binds a real TCP listener; run in a socket-capable environment"]
    async fn a_normal_close_reasoned_run_complete_stops_the_loop() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind stub listener");
        let port = listener.local_addr().expect("stub addr").port();
        let dials = Arc::new(AtomicUsize::new(0));
        let venue_dials = Arc::clone(&dials);
        let venue = tokio::spawn(async move {
            loop {
                let mut ws = accept_ws(&listener).await;
                venue_dials.fetch_add(1, Ordering::Relaxed);
                drop(
                    ws.close(Some(tokio_tungstenite::tungstenite::protocol::CloseFrame {
                        code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Normal,
                        reason: mogwai_protocol::close::RUN_COMPLETE.into(),
                    }))
                    .await,
                );
            }
        });
        let conn = ConnHavoc {
            reconnect_delay_initial_ms: 10,
            reconnect_delay_max_ms: 20,
            reconnect_backoff_factor: 1.0,
            reconnect_max_attempts: Some(3),
            ..Default::default()
        };
        tokio::time::timeout(Duration::from_secs(5), run_lifecycle(port, conn))
            .await
            .expect("a reasoned completion close ends the loop");
        assert_eq!(
            dials.load(Ordering::Relaxed),
            1,
            "the reasoned completion close is terminal: no redial follows it"
        );
        venue.abort();
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "binds a real TCP listener; run in a socket-capable environment"]
    async fn reconnect_attempt_counter_resets_once_a_frame_arrives() {
        // The attempt counter resets only once a connection proves itself by
        // delivering an inbound application frame. A venue that serves one
        // frame per connection before dropping keeps proving itself, so with a
        // two-attempt cap the loop must reconnect indefinitely - a counter
        // that never reset would exhaust after two cycles, and one that reset
        // on the bare dial is pinned out by the accept-then-die test above.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind stub listener");
        let port = listener.local_addr().expect("stub addr").port();
        let dials = Arc::new(AtomicUsize::new(0));
        let venue_dials = Arc::clone(&dials);
        let venue = tokio::spawn(async move {
            loop {
                let mut ws = accept_ws(&listener).await;
                venue_dials.fetch_add(1, Ordering::Relaxed);
                let frame = r#"{"type":"Heartbeat","ts_event":1}"#;
                drop(ws.send(Message::Text(frame.into())).await);
                drop(ws.close(None).await);
            }
        });

        let conn = ConnHavoc {
            reconnect_delay_initial_ms: 10,
            reconnect_delay_max_ms: 10,
            reconnect_backoff_factor: 1.0,
            reconnect_max_attempts: Some(2),
            ..Default::default()
        };
        let run = tokio::spawn(run_lifecycle(port, conn));

        let deadline = Instant::now() + Duration::from_secs(5);
        while dials.load(Ordering::Relaxed) < 5 {
            assert!(
                Instant::now() < deadline,
                "proven connections stopped reconnecting after {} dials",
                dials.load(Ordering::Relaxed)
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            !run.is_finished(),
            "a two-attempt cap must not exhaust across proven connections"
        );
        run.abort();
        venue.abort();
    }
}
