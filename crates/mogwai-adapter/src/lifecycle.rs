// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::Context;
use futures_util::{SinkExt, StreamExt};
use mogwai_protocol::{ClientMessage, ConnHavoc, ServerMessage, SimClock};
use rand::{RngExt, SeedableRng, rngs::StdRng};
use tokio::{
    sync::{
        Mutex,
        mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
    },
    time::{Instant, MissedTickBehavior, Sleep},
};
use tokio_tungstenite::{connect_async, tungstenite::Message};

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
        // the reconnect loop (D.3). The protocol validator now rejects BOTH
        // mixed-zero combinations - a positive initial with a zero max, and
        // (because the base is `initial * factor^attempt`) a zero initial with
        // a positive max, the same spin from the other side - so the only
        // validated config that yields a zero delay here is both bounds zero:
        // backoff deliberately disabled, nothing to spin against. The `max ==
        // 0` branch below stays as belt-and-suspenders for a caller that
        // bypasses the validator; the mirror case (zero initial, positive max)
        // deliberately gets NO defensive floor - the mechanism has no
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
/// counter, and returns whether the ADVANCED counter has hit
/// `reconnect_max_attempts`. When it has, the sleep is SKIPPED and `true` is
/// returned: the caller must return instead of looping, because the loop-top
/// exhausted check would return anyway and sleeping a final backoff first only
/// delays that exhausted return by one pointless interval (F12). When the cap
/// is not yet reached, the current attempt's backoff is slept and `false` is
/// returned so the caller re-dials. `connected` is already stored `false` on
/// both reconnect paths, so an exhausted return needs no further store here.
async fn backoff_or_exhausted(
    policy: &ReconnectPolicy,
    attempt: &mut u32,
    rng: &mut StdRng,
) -> bool {
    let delay = policy.backoff(*attempt, rng);
    *attempt = attempt.saturating_add(1);
    if policy.exhausted(*attempt) {
        return true;
    }
    tokio::time::sleep(delay).await;
    false
}

/// Per-connection HTTP rate limiter enforcing `max_requests_per_second`.
///
/// Accounting contract - which HTTP calls this meters, and which are exempt
/// (AE13). Keep call sites consistent with this list, since the meter itself
/// cannot see who bypasses it:
///
/// METERED (steady-state data-plane and order-plane). Every
/// `fetch_instruments` / `fetch_account` / `fetch_trades` / `post_order` in
/// `client.rs` awaits `wait()` before issuing its request, so the configured
/// ceiling governs the ongoing request stream a running strategy generates.
///
/// EXEMPT (connect-time bootstrap, deliberately un-metered):
///   - the clock fetch (`clock::fetch_clock`) - it runs BEFORE this quota
///     exists, because the quota's spacing is `sim`-scaled and `sim` is exactly
///     what the clock fetch returns; `connect()` builds the quota FROM that
///     result. Metering it is a chicken-and-egg: there is no quota to wait on
///     yet.
///   - `ship_server_havoc` - a bounded, one-shot control-plane POST loop that
///     arms divergences at connect, not part of the steady request stream.
///
/// Both exemptions are connect-time and bounded, so neither contributes to the
/// sustained rate the ceiling exists to bound. Routing them through the meter
/// too would be strictly consistent but is a `client.rs` call-site change, not
/// something this type can enforce.
///
/// Known shape, not fixed here: `wait()` holds the mutex across its sleep to
/// enforce FIFO spacing across concurrent callers, so a burst of N dispatches
/// queues linearly with no cap and no timeout. An order can sit in the queue
/// while nautilus sees only `Submitted`. A queue cap/timeout would be a design
/// change; the spacing-via-held-mutex is intentional.
#[derive(Clone, Debug)]
pub(crate) struct HttpQuota {
    min_interval: Option<Duration>,
    last_send: Arc<Mutex<Option<Instant>>>,
}

impl HttpQuota {
    pub(crate) fn from_conn(conn: &ConnHavoc, sim: SimClock) -> Self {
        Self {
            min_interval: conn.max_requests_per_second.map(|max| {
                // Round the per-request spacing UP (ceil-divide) so a rate that
                // does not evenly divide one second (e.g. 3/s -> 333_333_334ns,
                // not the floored 333_333_333 that ships a hair fast) never
                // undershoots the spacing and ships marginally above the cap
                // (D.9). Ceil keeps the effective rate at or below the
                // configured ceiling.
                let max = u64::from(max);
                let nanos = 1_000_000_000u64.div_ceil(max);
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
    pub(crate) sim: SimClock,
}

pub(crate) async fn run_ws_connection<
    Cmd,
    Serialize,
    OnConnect,
    Handler,
    HandlerFut,
    Disconnect,
    DisconnectFut,
>(
    config: WsConnectionConfig,
    mut cmd_rx: UnboundedReceiver<Cmd>,
    serialize: Serialize,
    on_connect: OnConnect,
    mut handler: Handler,
    mut on_disconnect: Disconnect,
) where
    Cmd: Send + 'static,
    Serialize: Fn(Cmd) -> ClientMessage + Send + Sync + 'static,
    OnConnect: Fn() -> Vec<Cmd> + Send + Sync + 'static,
    Handler: FnMut(ServerMessage) -> HandlerFut + Send + 'static,
    HandlerFut: Future<Output = ()> + Send,
    Disconnect: FnMut() -> DisconnectFut + Send + 'static,
    DisconnectFut: Future<Output = ()> + Send,
{
    let WsConnectionConfig {
        ws_url,
        conn,
        seed,
        connected,
        sim,
    } = config;
    let policy = ReconnectPolicy::from_conn(&conn, sim);
    // The reconnect-jitter RNG is seeded from the configured havoc seed when one
    // is set, so jitter is reproducible (D.6). Both client.rs construction sites
    // pass `seed: client_havoc.seed` into `WsConnectionConfig`, so a configured
    // seed reaches here; absent a seed we fall back to entropy.
    let mut rng = seed.map_or_else(|| StdRng::from_rng(&mut rand::rng()), StdRng::seed_from_u64);
    let mut attempt = 0u32;

    loop {
        if policy.exhausted(attempt) {
            connected.store(false, Ordering::Relaxed);
            return;
        }

        let ws = match connect_async(&ws_url).await {
            Ok((ws, _)) => ws,
            Err(_) => {
                connected.store(false, Ordering::Relaxed);
                if backoff_or_exhausted(&policy, &mut attempt, &mut rng).await {
                    return;
                }
                continue;
            }
        };

        // A successful dial alone does NOT reset the attempt counter: a venue
        // that accepts the handshake and immediately dies would otherwise
        // restart the count on every cycle, so `reconnect_max_attempts` could
        // never trip and each re-dial would re-enter at the cheap initial
        // backoff. The counter resets only once the connection proves itself
        // by delivering an inbound application frame (Text/Binary, in the
        // select loop below) - the same liveness criterion the idle timeout
        // uses - so unproven connect/teardown cycles keep walking the
        // exponential ladder toward the cap.
        connected.store(true, Ordering::Relaxed);
        let (mut writer, mut reader) = ws.split();
        let (out_tx, mut out_rx) = unbounded_channel::<Message>();
        let writer_handle = tokio::spawn(async move {
            while let Some(msg) = out_rx.recv().await {
                if writer.send(msg).await.is_err() {
                    break;
                }
            }
        });
        let (in_tx, mut in_rx) = unbounded_channel();
        let reader_handle = tokio::spawn(async move {
            while let Some(msg) = reader.next().await {
                if in_tx.send(Some(msg)).is_err() {
                    return;
                }
            }
            drop(in_tx.send(None));
        });

        for cmd in on_connect() {
            if send_command(&out_tx, &serialize, cmd).is_err() {
                break;
            }
        }

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
                cmd = cmd_rx.recv(), if !commands_closed => WsAction::Command(cmd),
                () = heartbeat_tick(&mut heartbeat), if heartbeat.is_some() => WsAction::Heartbeat,
                () = idle_tick(&mut idle_sleep), if idle_sleep.is_some() => WsAction::Idle,
            };

            match action {
                WsAction::Inbound(inbound) => {
                    if inbound_is_disconnected(&inbound) {
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
                            match serde_json::from_str::<ServerMessage>(&text) {
                                Ok(server_msg) => handler(server_msg).await,
                                Err(error) => tracing::warn!(
                                    %error,
                                    "dropping unparseable text server frame"
                                ),
                            }
                        }
                        Ok(Message::Binary(bytes)) => {
                            // Connection proven; same as the Text arm above.
                            attempt = 0;
                            reset_idle(&mut idle_sleep, conn.idle_timeout_ms, sim);
                            match serde_json::from_slice::<ServerMessage>(&bytes) {
                                Ok(server_msg) => handler(server_msg).await,
                                Err(error) => tracing::warn!(
                                    %error,
                                    "dropping unparseable binary server frame"
                                ),
                            }
                        }
                        Ok(Message::Ping(payload)) => {
                            if out_tx.send(Message::Pong(payload)).is_err() {
                                break;
                            }
                        }
                        Ok(_) => {}
                        Err(_) => unreachable!("inbound errors returned above"),
                    }
                }
                WsAction::Command(Some(cmd)) => {
                    if send_command(&out_tx, &serialize, cmd).is_err() {
                        break;
                    }
                }
                WsAction::Command(None) => {
                    commands_closed = true;
                }
                WsAction::Heartbeat => {
                    if out_tx.send(Message::Ping(Vec::new().into())).is_err() {
                        break;
                    }
                }
                WsAction::Idle => break,
            }
        }

        // Abort then join: aborting only requests cancellation, so awaiting the
        // handles makes the writer/reader tasks observably quiesced before the
        // next reconnect iteration spins up a fresh socket and a new pair of
        // tasks (D.12). Without the join an in-flight writer send could still be
        // racing the teardown. A `JoinError` here is expected - the task was
        // just aborted - so it is intentionally ignored.
        writer_handle.abort();
        reader_handle.abort();
        drop(writer_handle.await);
        drop(reader_handle.await);
        on_disconnect().await;
        connected.store(false, Ordering::Relaxed);
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
        if backoff_or_exhausted(&policy, &mut attempt, &mut rng).await {
            return;
        }
    }
}

fn inbound_is_disconnected(
    inbound: &Option<Result<Message, tokio_tungstenite::tungstenite::Error>>,
) -> bool {
    if inbound.is_none() {
        return true;
    }
    if inbound.as_ref().is_some_and(Result::is_err) {
        return true;
    }
    inbound
        .as_ref()
        .is_some_and(|msg| matches!(msg, Ok(Message::Close(_))))
}

fn send_command<Cmd, Serialize>(
    out_tx: &UnboundedSender<Message>,
    serialize: &Serialize,
    cmd: Cmd,
) -> anyhow::Result<()>
where
    Serialize: Fn(Cmd) -> ClientMessage,
{
    let msg = serialize(cmd);
    let payload = serde_json::to_string(&msg).context("encode websocket command")?;
    out_tx
        .send(Message::Text(payload.into()))
        .context("send websocket command")
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

    /// Completes one server-side WS handshake on `listener`.
    async fn accept_ws(
        listener: &tokio::net::TcpListener,
    ) -> tokio_tungstenite::WebSocketStream<tokio::net::TcpStream> {
        let (stream, _) = listener.accept().await.expect("accept ws dial");
        tokio_tungstenite::accept_async(stream)
            .await
            .expect("server ws handshake")
    }

    /// Drives `run_ws_connection` with inert callbacks against a loopback stub.
    async fn run_lifecycle(port: u16, conn: ConnHavoc) {
        let (_cmd_tx, cmd_rx) = unbounded_channel::<ClientMessage>();
        run_ws_connection(
            WsConnectionConfig {
                ws_url: format!("ws://127.0.0.1:{port}/ws"),
                conn,
                seed: Some(1),
                connected: Arc::new(AtomicBool::new(false)),
                sim: SimClock::identity(),
            },
            cmd_rx,
            |cmd: ClientMessage| cmd,
            Vec::new,
            |_msg: ServerMessage| async {},
            || async {},
        )
        .await;
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
        let server_dials = Arc::clone(&dials);
        let server = tokio::spawn(async move {
            loop {
                let mut ws = accept_ws(&listener).await;
                server_dials.fetch_add(1, Ordering::Relaxed);
                drop(ws.close(None).await);
            }
        });

        let conn = ConnHavoc {
            reconnect_delay_initial_ms: 100,
            reconnect_delay_max_ms: 100,
            reconnect_backoff_factor: 1.0,
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
            (Duration::from_millis(200)..Duration::from_millis(300)).contains(&elapsed),
            "three dials span exactly two 100ms backoffs (~200ms): the first two \
             unproven drops sleep before redialing, but the third, exhausting \
             cycle returns immediately instead of sleeping a pointless final \
             backoff (F12) - a third sleep would push elapsed to ~300ms \
             (elapsed {elapsed:?})"
        );
        server.abort();
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
        let server_dials = Arc::clone(&dials);
        let server = tokio::spawn(async move {
            loop {
                let mut ws = accept_ws(&listener).await;
                server_dials.fetch_add(1, Ordering::Relaxed);
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
        server.abort();
    }
}
