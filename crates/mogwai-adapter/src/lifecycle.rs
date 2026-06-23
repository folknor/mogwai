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
use mogwai_protocol::{ClientMessage, ConnHavoc, ServerMessage};
use rand::{Rng, SeedableRng, rngs::StdRng};
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
    initial: Duration,
    max: Duration,
    factor: f64,
    jitter_ms: u64,
    max_attempts: Option<u32>,
}

impl ReconnectPolicy {
    pub(crate) fn from_conn(conn: &ConnHavoc) -> Self {
        Self {
            initial: Duration::from_millis(conn.reconnect_delay_initial_ms),
            max: Duration::from_millis(conn.reconnect_delay_max_ms),
            factor: conn.reconnect_backoff_factor,
            jitter_ms: conn.reconnect_jitter_ms,
            max_attempts: conn.reconnect_max_attempts,
        }
    }

    pub(crate) fn backoff(&self, attempt: u32, rng: &mut StdRng) -> Duration {
        let initial_ms = self.initial.as_millis() as f64;
        let max_ms = self.max.as_millis() as f64;
        let base_ms = if initial_ms == 0.0 || max_ms == 0.0 {
            0.0
        } else {
            (initial_ms * self.factor.powi(attempt as i32)).min(max_ms)
        };
        let jitter = if self.jitter_ms == 0 {
            0
        } else {
            rng.gen_range(0..=self.jitter_ms)
        };
        Duration::from_millis(base_ms as u64).saturating_add(Duration::from_millis(jitter))
    }

    pub(crate) fn exhausted(&self, attempt: u32) -> bool {
        self.max_attempts.is_some_and(|max| attempt >= max)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct HttpQuota {
    min_interval: Option<Duration>,
    last_send: Arc<Mutex<Option<Instant>>>,
}

impl HttpQuota {
    pub(crate) fn from_conn(conn: &ConnHavoc) -> Self {
        Self {
            min_interval: conn.max_requests_per_second.map(|max| {
                let nanos = 1_000_000_000u64 / u64::from(max);
                Duration::from_nanos(nanos.max(1))
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
    } = config;
    let policy = ReconnectPolicy::from_conn(&conn);
    let mut rng = seed.map_or_else(StdRng::from_entropy, StdRng::seed_from_u64);
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
                let delay = policy.backoff(attempt, &mut rng);
                attempt = attempt.saturating_add(1);
                tokio::time::sleep(delay).await;
                continue;
            }
        };

        attempt = 0;
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
            let mut interval =
                tokio::time::interval(Duration::from_millis(conn.heartbeat_interval_ms));
            interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
            interval
        });
        let mut idle_sleep = idle_sleep(conn.idle_timeout_ms);
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
                            reset_idle(&mut idle_sleep, conn.idle_timeout_ms);
                            if let Ok(server_msg) = serde_json::from_str::<ServerMessage>(&text) {
                                handler(server_msg).await;
                            }
                        }
                        Ok(Message::Binary(bytes)) => {
                            reset_idle(&mut idle_sleep, conn.idle_timeout_ms);
                            if let Ok(server_msg) = serde_json::from_slice::<ServerMessage>(&bytes)
                            {
                                handler(server_msg).await;
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

        writer_handle.abort();
        reader_handle.abort();
        on_disconnect().await;
        connected.store(false, Ordering::Relaxed);
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

fn idle_sleep(timeout_ms: u64) -> Option<Pin<Box<Sleep>>> {
    (timeout_ms > 0).then(|| Box::pin(tokio::time::sleep(Duration::from_millis(timeout_ms))))
}

fn reset_idle(idle_sleep: &mut Option<Pin<Box<Sleep>>>, timeout_ms: u64) {
    if let Some(sleep) = idle_sleep {
        sleep
            .as_mut()
            .reset(Instant::now() + Duration::from_millis(timeout_ms));
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
    use super::*;

    #[test]
    fn reconnect_policy_backoff_grows_and_clamps() {
        let conn = ConnHavoc {
            reconnect_delay_initial_ms: 100,
            reconnect_delay_max_ms: 1_000,
            reconnect_backoff_factor: 2.0,
            ..Default::default()
        };
        let policy = ReconnectPolicy::from_conn(&conn);
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
        let policy = ReconnectPolicy::from_conn(&conn);
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
    fn reconnect_policy_exhausted_flips_at_cap() {
        let conn = ConnHavoc {
            reconnect_max_attempts: Some(3),
            ..Default::default()
        };
        let policy = ReconnectPolicy::from_conn(&conn);

        assert!(!policy.exhausted(0));
        assert!(!policy.exhausted(2));
        assert!(policy.exhausted(3));
    }

    #[test]
    fn conn_reconnect_policy_respects_max_attempts() {
        let conn = ConnHavoc {
            reconnect_max_attempts: Some(2),
            ..Default::default()
        };
        let policy = ReconnectPolicy::from_conn(&conn);

        assert!(!policy.exhausted(0));
        assert!(!policy.exhausted(1));
        assert!(policy.exhausted(2));
    }

    #[tokio::test]
    async fn conn_http_quota_spaces_requests() {
        let quota = HttpQuota::from_conn(&ConnHavoc {
            max_requests_per_second: Some(20),
            ..Default::default()
        });

        quota.wait().await;
        let first = Instant::now();
        quota.wait().await;
        let elapsed = first.elapsed();

        assert!(
            elapsed >= Duration::from_millis(50),
            "quota allowed second request after {elapsed:?}"
        );
    }
}
