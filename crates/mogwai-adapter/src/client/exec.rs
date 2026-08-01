// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! `MogwaiExecutionClient`: the `ExecutionClient` half of the adapter. Owns
//! the order/fill/position mirror (`ExecState`), the HTTP-or-WS order
//! dispatch, the venue-message-to-nautilus-event translation, and the
//! report generators startup reconciliation consumes. Plumbing shared with
//! the data half (the havoc dispatch pipeline, the instrument cache,
//! clock/url glue) lives in `super::shared`.

use std::{
    collections::HashMap,
    str::FromStr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use anyhow::{Context, ensure};
use async_trait::async_trait;
use mogwai_protocol::{
    ClientMessage, FillSnapshot, HavocSpec, InstrumentDef, OrderStatusInfo, OrderStatusSnapshot,
    ServerMessage, SimClock, Symbol,
};
use nautilus_common::{
    clients::ExecutionClient,
    live::{get_runtime, try_get_exec_event_sender},
    messages::execution::{
        CancelOrder, GenerateFillReports, GenerateOrderStatusReport, GenerateOrderStatusReports,
        GeneratePositionStatusReports, ModifyOrder, QueryOrder, SubmitOrder,
    },
};
use nautilus_core::{UUID4, UnixNanos, time::get_atomic_clock_realtime};
use nautilus_live::{ExecutionClientCore, ExecutionEventEmitter};
use nautilus_model::{
    accounts::AccountAny,
    enums::{LiquiditySide, OmsType, OrderSide, OrderStatus, OrderType, PositionSideSpecified},
    events::{
        AccountState as NautilusAccountState, OrderAccepted, OrderCancelRejected, OrderCanceled,
        OrderEventAny, OrderFilled, OrderModifyRejected, OrderRejected, OrderSubmitted,
        OrderUpdated,
    },
    identifiers::{AccountId, ClientId, ClientOrderId, InstrumentId, TradeId, Venue, VenueOrderId},
    orders::Order,
    reports::{ExecutionMassStatus, FillReport, OrderStatusReport, PositionStatusReport},
    types::{AccountBalance, MarginBalance, currency::Currency},
};
use nautilus_network::http::HttpClient;
use rust_decimal::Decimal;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::task::JoinHandle;

use crate::{
    MOGWAI_VENUE, MogwaiExecClientConfig,
    client::shared::{
        HavocDelivery, HavocFilter, abort_tasks, client_havoc, client_havoc_for_dispatch,
        conn_havoc, dispatch_havoc, enqueue_havoc, fetch_clock_or_identity, flush_havoc,
        flush_havoc_into_pump, instrument_def, join_url, lock_recover, now_unix_nanos,
        request_timeout_secs, seed_instruments, spawn_latency_pump, symbol_from_instrument,
        track_task, wait_connected,
    },
    convert,
    lifecycle::{HttpQuota, WsConnectionConfig, run_ws_connection},
};

const ACCOUNT_REGISTRATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const ACCOUNT_REGISTRATION_POLL: std::time::Duration = std::time::Duration::from_millis(10);

/// Distinguishes a 404 (older server without GET /account, the only
/// warn-and-continue case) from every other pull failure (decode, 5xx, timeout,
/// transport), which must fail connect() rather than silently recreate the
/// first-fill `account not found in cache` this fix exists to eliminate.
#[derive(Debug)]
enum FetchAccountError {
    NotFound,
    Other(anyhow::Error),
}

impl std::fmt::Display for FetchAccountError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "fetch account returned 404"),
            Self::Other(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for FetchAccountError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NotFound => None,
            Self::Other(err) => err.source(),
        }
    }
}

impl From<anyhow::Error> for FetchAccountError {
    fn from(err: anyhow::Error) -> Self {
        Self::Other(err)
    }
}

async fn fetch_account(
    http: &HttpClient,
    quota: &HttpQuota,
    base: &str,
) -> Result<mogwai_protocol::AccountState, FetchAccountError> {
    quota.wait().await;
    let response = http
        .get(
            join_url(base, "account"),
            None,
            None,
            Some(mogwai_protocol::DEFAULT_REQUEST_TIMEOUT_SECS),
            None,
        )
        .await
        .context("fetch account")?;
    if response.status.as_u16() == 404 {
        return Err(FetchAccountError::NotFound);
    }
    if !response.status.is_success() {
        return Err(FetchAccountError::Other(anyhow::anyhow!(
            "fetch account returned {}",
            response.status.as_u16()
        )));
    }
    serde_json::from_slice(&response.body)
        .context("decode account")
        .map_err(FetchAccountError::Other)
}

fn validate_account_snapshot(
    state: &mogwai_protocol::AccountState,
    expected: AccountId,
) -> anyhow::Result<()> {
    ensure!(
        state.account_id.as_str() == expected.as_ref(),
        "account snapshot belongs to {}, expected {}",
        state.account_id.as_str(),
        expected,
    );
    Ok(())
}
async fn post_order(
    http: &HttpClient,
    quota: &HttpQuota,
    url: &str,
    msg: &ClientMessage,
    timeout_secs: u64,
) -> anyhow::Result<Vec<ServerMessage>> {
    let body = serde_json::to_vec(msg).context("encode order")?;
    let mut headers = HashMap::new();
    headers.insert("content-type".to_string(), "application/json".to_string());
    quota.wait().await;
    let response = http
        .post(
            url.to_string(),
            None,
            Some(headers),
            Some(body),
            Some(timeout_secs),
            None,
        )
        .await
        .context("post order")?;
    ensure!(
        response.status.is_success(),
        "post order returned {}",
        response.status.as_u16()
    );
    serde_json::from_slice(&response.body).context("decode order events")
}
async fn ship_server_havoc(
    http: &HttpClient,
    http_base: &str,
    spec: &HavocSpec,
    timeout_secs: u64,
) -> anyhow::Result<()> {
    let url = join_url(http_base, "control/divergence");
    for divergence in &spec.server {
        let body = serde_json::to_vec(divergence).context("encode divergence")?;
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        let response = http
            .post(
                url.clone(),
                None,
                Some(headers),
                Some(body),
                Some(timeout_secs),
                None,
            )
            .await
            .context("post divergence")?;
        ensure!(
            response.status.is_success(),
            "post divergence returned {}",
            response.status.as_u16()
        );
    }
    Ok(())
}
#[derive(Debug)]
pub struct MogwaiExecutionClient {
    core: ExecutionClientCore,
    config: MogwaiExecClientConfig,
    emitter: ExecutionEventEmitter,
    connected: Arc<AtomicBool>,
    http: HttpClient,
    http_quota: HttpQuota,
    sim: SimClock,
    ws_cmd: Option<UnboundedSender<ExecWsCommand>>,
    state: Arc<Mutex<ExecState>>,
    instruments: Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    /// In-flight `QueryOrders`/`QueryFills` waiters keyed by correlation id.
    /// The WS reader (via `handle_exec_message`) resolves each waiter when
    /// the venue's snapshot reply lands; `stop()` drains the maps so a waiter
    /// blocked on a dead socket errors out instead of waiting out its timeout.
    pending: Arc<Mutex<PendingQueries>>,
    http_dispatch_counter: Arc<AtomicU64>,
    /// Handles for the WS reader and every spawned HTTP order dispatch. Shared
    /// behind an `Arc<Mutex<..>>` so the `&self` `dispatch_order` can record its
    /// spawned POST task; `stop()` aborts the lot so a slow POST cannot emit exec
    /// events (its emitter still holds a live sender clone) after the client
    /// stopped (AE19).
    task_handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl MogwaiExecutionClient {
    /// Creates a new disconnected mogwai execution client.
    ///
    /// # Errors
    ///
    /// Returns an error if the supplied config is invalid.
    pub fn new(core: ExecutionClientCore, config: MogwaiExecClientConfig) -> anyhow::Result<Self> {
        config.validate()?;
        let emitter = ExecutionEventEmitter::new(
            get_atomic_clock_realtime(),
            config.trader_id,
            config.account_id,
            config.account_type,
            None,
        );
        let http = HttpClient::new(
            HashMap::from([(
                mogwai_protocol::ACCOUNT_HEADER.to_string(),
                config.account_id.to_string(),
            )]),
            Vec::new(),
            Vec::new(),
            None,
            Some(mogwai_protocol::DEFAULT_REQUEST_TIMEOUT_SECS),
            None,
        )
        .context("create HTTP client")?;
        Ok(Self {
            core,
            http_quota: HttpQuota::from_conn(&conn_havoc(&config.havoc), SimClock::identity()),
            config,
            emitter,
            connected: Arc::new(AtomicBool::new(false)),
            http,
            sim: SimClock::identity(),
            ws_cmd: None,
            state: Arc::new(Mutex::new(ExecState::default())),
            instruments: Arc::new(Mutex::new(HashMap::new())),
            pending: Arc::new(Mutex::new(PendingQueries::default())),
            http_dispatch_counter: Arc::new(AtomicU64::new(0)),
            task_handles: Arc::new(Mutex::new(Vec::new())),
        })
    }

    #[cfg(test)]
    fn is_started(&self) -> bool {
        self.core.is_started()
    }

    #[cfg(test)]
    fn is_stopped(&self) -> bool {
        self.core.is_stopped()
    }

    fn send_ws(&self, cmd: ExecWsCommand) -> anyhow::Result<()> {
        let tx = self
            .ws_cmd
            .as_ref()
            .context("mogwai execution client is not connected")?;
        tx.send(cmd).context("send execution websocket command")
    }

    fn exec_context(&self) -> ExecContext {
        ExecContext {
            emitter: self.emitter.clone(),
            state: Arc::clone(&self.state),
            instruments: Arc::clone(&self.instruments),
            pending: Arc::clone(&self.pending),
            trader_id: self.core.trader_id,
            account_id: self.core.account_id,
            account_type: self.config.account_type,
            sim: self.sim,
        }
    }

    fn dispatch_order(&self, cmd: ExecWsCommand) -> anyhow::Result<()> {
        if self.config.transport_profile.orders_over_http() {
            let msg = exec_command_to_client_message(cmd.clone());
            let http = self.http.clone();
            let http_quota = self.http_quota.clone();
            let url = join_url(&self.config.http_base_url(), "orders");
            let ctx = self.exec_context();
            let counter = self.http_dispatch_counter.fetch_add(1, Ordering::Relaxed);
            let client_havoc = client_havoc_for_dispatch(&self.config.havoc, counter);
            let timeout_secs = request_timeout_secs(&self.config.havoc, self.sim);
            track_task(
                &self.task_handles,
                get_runtime().spawn(async move {
                    let mut filter = HavocFilter::from_client(&client_havoc);
                    match post_order(&http, &http_quota, &url, &msg, timeout_secs).await {
                        Ok(events) => {
                            for event in events {
                                dispatch_havoc(&mut filter, event, ctx.sim, |msg| async {
                                    handle_exec_message(msg, &ctx);
                                })
                                .await;
                            }
                            flush_havoc(&mut filter, ctx.sim, |msg| async {
                                handle_exec_message(msg, &ctx);
                            })
                            .await;
                        }
                        Err(err) => synthesize_transport_reject(&cmd, &err, &ctx),
                    }
                }),
            );
            Ok(())
        } else if let Err(err) = self.send_ws(cmd.clone()) {
            // The WS command channel is gone (reconnect exhausted, or the client
            // was stopped), so the command never reached the venue. Nautilus only
            // LOGS an Err from cancel_order/modify_order (no event), so without
            // this a cancel/modify would sit forever in PendingCancel/PendingUpdate
            // (and a submit in Submitted) with no reject to restore it - unlike the
            // HTTP path, which already synthesizes the matching reject on transport
            // failure. Synthesize it here too and report success: the reject event
            // is the signal, not the return value (matching the HTTP path, whose
            // spawn returns Ok and surfaces the failure only as the event) (AE9).
            let ctx = self.exec_context();
            synthesize_transport_reject(&cmd, &err, &ctx);
            Ok(())
        } else {
            Ok(())
        }
    }

    /// Block until the runner has drained the forwarded AccountState and the
    /// cache holds the account row, or the timeout elapses. The forward only
    /// queues an event; this proves the row is present before connect() returns
    /// so the first order is not worked against an unknown account.
    async fn await_account_registered(&self) -> anyhow::Result<()> {
        let deadline = tokio::time::Instant::now() + ACCOUNT_REGISTRATION_TIMEOUT;
        loop {
            if self
                .core
                .cache()
                .account_owned(&self.core.account_id)
                .is_some()
            {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!(
                    "account {} not registered after {:?}",
                    self.core.account_id,
                    ACCOUNT_REGISTRATION_TIMEOUT
                );
            }
            tokio::time::sleep(ACCOUNT_REGISTRATION_POLL).await;
        }
    }

    /// Snapshot the transport pieces one venue-truth query needs, cloneable
    /// into spawned tasks (`query_order`). Built per call so it always sees
    /// the current WS command channel and havoc-scaled timeout.
    fn venue_query(&self) -> VenueQuery {
        let http = self
            .config
            .transport_profile
            .orders_over_http()
            .then(|| HttpQueryTransport {
                http: self.http.clone(),
                quota: self.http_quota.clone(),
                url: join_url(&self.config.http_base_url(), "orders"),
            });
        VenueQuery {
            http,
            ws_cmd: self.ws_cmd.clone(),
            pending: Arc::clone(&self.pending),
            timeout_secs: request_timeout_secs(&self.config.havoc, self.sim),
        }
    }
}

/// The request/reply transport for the venue-truth queries. Under an HTTP
/// orders profile the venue answers synchronously in the `POST /orders`
/// response body; under WS the query is correlated to its reply through
/// `pending` by the echoed `request_id`, since the reply shares the socket
/// with unsolicited execution events.
///
/// Delivery is havoc-able, content is not: the reply is always a truthful
/// engine book read, but on the WS carrier it classifies as execution, so an
/// armed `DelayAcks` holds it and `GoDark` drops it - a lost or late reply
/// surfaces here as a timeout, exercising the consumer's query-timeout path
/// without ever letting havoc alter what the venue says.
#[derive(Clone)]
struct VenueQuery {
    http: Option<HttpQueryTransport>,
    ws_cmd: Option<UnboundedSender<ExecWsCommand>>,
    pending: Arc<Mutex<PendingQueries>>,
    timeout_secs: u64,
}

#[derive(Clone)]
struct HttpQueryTransport {
    http: HttpClient,
    quota: HttpQuota,
    url: String,
}

impl VenueQuery {
    async fn order_status(
        &self,
        client_order_id: Option<String>,
        open_only: bool,
    ) -> anyhow::Result<OrderStatusSnapshot> {
        let request_id = UUID4::new().to_string();
        if let Some(transport) = &self.http {
            let msg = ClientMessage::QueryOrders {
                request_id: request_id.clone(),
                client_order_id,
                open_only,
            };
            let events = post_order(
                &transport.http,
                &transport.quota,
                &transport.url,
                &msg,
                self.timeout_secs,
            )
            .await?;
            return events
                .into_iter()
                .find_map(|event| match event {
                    ServerMessage::OrderStatusSnapshot(snapshot)
                        if snapshot.request_id == request_id =>
                    {
                        Some(snapshot)
                    }
                    _ => None,
                })
                .context("venue reply carried no matching order status snapshot");
        }
        let reply_rx = self.register_ws_query(
            |pending, tx| drop(pending.orders.insert(request_id.clone(), tx)),
            ExecWsCommand::QueryOrders {
                request_id: request_id.clone(),
                client_order_id,
                open_only,
            },
        )?;
        self.await_reply(reply_rx, &request_id, |pending, id| {
            pending.orders.remove(id);
        })
        .await
    }

    async fn fill_history(&self, client_order_id: Option<String>) -> anyhow::Result<FillSnapshot> {
        let request_id = UUID4::new().to_string();
        if let Some(transport) = &self.http {
            let msg = ClientMessage::QueryFills {
                request_id: request_id.clone(),
                client_order_id,
            };
            let events = post_order(
                &transport.http,
                &transport.quota,
                &transport.url,
                &msg,
                self.timeout_secs,
            )
            .await?;
            return events
                .into_iter()
                .find_map(|event| match event {
                    ServerMessage::FillSnapshot(snapshot) if snapshot.request_id == request_id => {
                        Some(snapshot)
                    }
                    _ => None,
                })
                .context("venue reply carried no matching fill snapshot");
        }
        let reply_rx = self.register_ws_query(
            |pending, tx| drop(pending.fills.insert(request_id.clone(), tx)),
            ExecWsCommand::QueryFills {
                request_id: request_id.clone(),
                client_order_id,
            },
        )?;
        self.await_reply(reply_rx, &request_id, |pending, id| {
            pending.fills.remove(id);
        })
        .await
    }

    /// Register the waiter BEFORE sending the command, so a reply racing back
    /// faster than this task resumes still finds its slot; unregister on a
    /// send failure so a dead socket does not leak the entry.
    fn register_ws_query<T>(
        &self,
        register: impl FnOnce(&mut PendingQueries, tokio::sync::oneshot::Sender<T>),
        cmd: ExecWsCommand,
    ) -> anyhow::Result<tokio::sync::oneshot::Receiver<T>> {
        let tx = self
            .ws_cmd
            .as_ref()
            .context("mogwai execution client is not connected")?;
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        register(
            &mut lock_recover(&self.pending, "pending queries"),
            reply_tx,
        );
        tx.send(cmd)
            .context("send execution websocket query command")?;
        Ok(reply_rx)
    }

    /// Await the correlated reply, bounding the wait by the (havoc-scaled)
    /// request timeout - the same ceiling the HTTP carrier gets from its
    /// client - and clean the pending slot up on timeout so a reply that
    /// straggles in later is logged as unsolicited rather than leaking.
    async fn await_reply<T>(
        &self,
        reply_rx: tokio::sync::oneshot::Receiver<T>,
        request_id: &str,
        unregister: impl FnOnce(&mut PendingQueries, &str),
    ) -> anyhow::Result<T> {
        let timeout = std::time::Duration::from_secs(self.timeout_secs.max(1));
        match tokio::time::timeout(timeout, reply_rx).await {
            Ok(Ok(snapshot)) => Ok(snapshot),
            // The sender was dropped without a reply: stop() drained the
            // pending map (client stopping), so fail fast.
            Ok(Err(_)) => anyhow::bail!("venue query abandoned: execution client stopped"),
            Err(_) => {
                unregister(
                    &mut lock_recover(&self.pending, "pending queries"),
                    request_id,
                );
                anyhow::bail!(
                    "venue query {request_id} timed out after {}s (reply delayed or dropped)",
                    self.timeout_secs.max(1)
                )
            }
        }
    }
}

/// In-flight venue-truth query waiters, correlation id -> reply sender.
#[derive(Debug, Default)]
struct PendingQueries {
    orders: HashMap<String, tokio::sync::oneshot::Sender<OrderStatusSnapshot>>,
    fills: HashMap<String, tokio::sync::oneshot::Sender<FillSnapshot>>,
}

/// Converts one venue-truth `QueryOrders` row into the nautilus report,
/// dropping the row with a warning when a wire value cannot represent - the
/// same discipline every other wire-to-nautilus conversion in this module
/// applies.
fn order_status_report_from_info(
    info: &OrderStatusInfo,
    instruments: &Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    account_id: AccountId,
    ts_init: UnixNanos,
) -> Option<OrderStatusReport> {
    let client_order_id = wire_client_order_id(&info.client_order_id)?;
    let venue_order_id = wire_venue_order_id(&info.venue_order_id)?;
    let Some(def) = instrument_def(instruments, &info.symbol) else {
        tracing::warn!(
            symbol = %info.symbol,
            order = %info.client_order_id,
            "dropping order status report: no instrument def (cache not seeded?)"
        );
        return None;
    };
    let convert_qty = |value, label| {
        convert::quantity(value, def.size_precision)
            .map_err(|err| {
                tracing::warn!(
                    order = %info.client_order_id,
                    field = label,
                    error = %err,
                    "dropping order status report: unrepresentable quantity"
                );
            })
            .ok()
    };
    let quantity = convert_qty(info.quantity, "quantity")?;
    let filled = convert_qty(info.filled_qty, "filled_qty")?;
    Some(OrderStatusReport::new(
        account_id,
        convert::instrument_id(&def),
        Some(client_order_id),
        venue_order_id,
        convert::nautilus_side(info.side),
        convert::nautilus_order_type(info.order_type),
        convert::nautilus_time_in_force(info.time_in_force),
        convert::nautilus_order_status(info.status),
        quantity,
        filled,
        UnixNanos::from(info.ts_accepted),
        UnixNanos::from(info.ts_last),
        ts_init,
        None,
    ))
}

/// Converts one venue-truth `QueryFills` row into the nautilus fill report,
/// with the same drop-and-warn discipline as `order_status_report_from_info`.
fn fill_report_from_wire(
    fill: &mogwai_protocol::OrderFilled,
    instruments: &Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    account_id: AccountId,
    ts_init: UnixNanos,
) -> Option<FillReport> {
    let client_order_id = wire_client_order_id(&fill.client_order_id)?;
    let venue_order_id = wire_venue_order_id(&fill.venue_order_id)?;
    let Some(def) = instrument_def(instruments, &fill.symbol) else {
        tracing::warn!(
            symbol = %fill.symbol,
            trade = %fill.trade_id,
            "dropping fill report: no instrument def (cache not seeded?)"
        );
        return None;
    };
    let warn_drop = |label: &str, err: &dyn std::fmt::Display| {
        tracing::warn!(
            trade = %fill.trade_id,
            field = label,
            error = %err,
            "dropping fill report: unrepresentable value"
        );
    };
    let quote_currency = match Currency::from_str(&def.quote) {
        Ok(currency) => currency,
        Err(err) => {
            warn_drop("quote currency", &err);
            return None;
        }
    };
    let trade_id = match TradeId::new_checked(&fill.trade_id) {
        Ok(trade_id) => trade_id,
        Err(err) => {
            warn_drop("trade_id", &err);
            return None;
        }
    };
    let last_qty = match convert::quantity(fill.last_qty, def.size_precision) {
        Ok(last_qty) => last_qty,
        Err(err) => {
            warn_drop("last_qty", &err);
            return None;
        }
    };
    let last_px = match convert::price(fill.last_px, def.price_precision) {
        Ok(last_px) => last_px,
        Err(err) => {
            warn_drop("last_px", &err);
            return None;
        }
    };
    let commission = match convert::money(fill.commission, quote_currency) {
        Ok(commission) => commission,
        Err(err) => {
            warn_drop("commission", &err);
            return None;
        }
    };
    // Taker unconditionally: the wire carries no maker/taker flag (see the
    // fill event handler's identical, deliberately lossy mapping).
    Some(FillReport::new(
        account_id,
        convert::instrument_id(&def),
        venue_order_id,
        trade_id,
        convert::nautilus_side(fill.side),
        last_qty,
        last_px,
        commission,
        LiquiditySide::Taker,
        Some(client_order_id),
        None,
        UnixNanos::from(fill.ts_event),
        ts_init,
        None,
    ))
}

#[async_trait(?Send)]
impl ExecutionClient for MogwaiExecutionClient {
    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    fn client_id(&self) -> ClientId {
        self.core.client_id
    }

    fn account_id(&self) -> AccountId {
        self.core.account_id
    }

    fn venue(&self) -> Venue {
        *MOGWAI_VENUE
    }

    fn oms_type(&self) -> OmsType {
        self.core.oms_type
    }

    fn get_account(&self) -> Option<AccountAny> {
        self.core.cache().account_owned(&self.core.account_id)
    }

    fn generate_account_state(
        &self,
        balances: Vec<AccountBalance>,
        margins: Vec<MarginBalance>,
        reported: bool,
        ts_event: UnixNanos,
    ) -> anyhow::Result<()> {
        self.emitter.send_account_state(NautilusAccountState::new(
            self.core.account_id,
            self.config.account_type,
            balances,
            margins,
            reported,
            UUID4::new(),
            ts_event,
            now_unix_nanos(self.sim),
            None,
        ));
        Ok(())
    }

    fn start(&mut self) -> anyhow::Result<()> {
        if self.core.is_started() {
            return Ok(());
        }

        if let Some(sender) = try_get_exec_event_sender() {
            self.emitter.set_sender(sender);
        }
        self.core.set_started();
        Ok(())
    }

    fn stop(&mut self) -> anyhow::Result<()> {
        if self.core.is_stopped() {
            return Ok(());
        }

        self.connected.store(false, Ordering::Relaxed);
        self.ws_cmd = None;
        abort_tasks(&self.task_handles);
        // Drop every in-flight query waiter's sender so a report generator
        // blocked on a reply over the now-dead socket errors out immediately
        // (oneshot RecvError) instead of waiting out its full timeout.
        {
            let mut pending = lock_recover(&self.pending, "pending queries");
            pending.orders.clear();
            pending.fills.clear();
        }
        self.core.set_stopped();
        self.core.set_disconnected();
        Ok(())
    }

    fn reset(&mut self) -> anyhow::Result<()> {
        // Mirror MogwaiDataClient::reset: stop first (abort tasks, drop the WS
        // command channel), then clear the reconciliation mirror. Without this
        // the default no-op `reset` leaves ExecState.orders and the account
        // staleness watermark populated across a stop/start, so a prior
        // session's orders leak into the next session's status/fill reports and
        // its watermark makes the new session's first account snapshot look
        // stale.
        self.stop()?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("execution state mutex poisoned"))?;
        *state = ExecState::default();
        Ok(())
    }

    async fn connect(&mut self) -> anyhow::Result<()> {
        let http_base_url = self.config.http_base_url();
        // The execution client rides only the affine map; the tape boundary in
        // the envelope is the data client's concern.
        let sim = fetch_clock_or_identity(&self.http, &http_base_url)
            .await
            .sim;
        self.sim = sim;
        let conn = conn_havoc(&self.config.havoc);
        self.http_quota = HttpQuota::from_conn(&conn, sim);
        seed_instruments(
            &self.http,
            &self.http_quota,
            &http_base_url,
            &self.instruments,
        )
        .await?;
        if let Some(havoc) = &self.config.havoc {
            // Same scaled timeout as dispatch_order: the configured
            // conn.request_timeout_secs, not the default (AD25).
            let timeout_secs = request_timeout_secs(&self.config.havoc, sim);
            ship_server_havoc(&self.http, &http_base_url, havoc, timeout_secs).await?;
        }
        // Seed the bridge's account row before any order is worked. Pull the
        // venue's current snapshot and forward it through the same exec dispatch
        // an inbound AccountState frame uses, so the cache row exists before the
        // first order's events arrive instead of erroring `account not found in
        // cache`. Instruments are already seeded above, so any positions in the
        // snapshot resolve their defs.
        //
        // Forwarding alone is necessary but not sufficient: handle_account_state
        // only sends an ExecutionEvent::Account onto the exec channel, which the
        // runner drains and applies to the cache asynchronously - possibly after
        // connect() returns and the first order is worked. So we block on
        // await_account_registered (mirroring every canonical adapter's
        // await_account_registered) until the cache row is proven present.
        //
        // This pull bypasses the HavocFilter that WS-pushed AccountState frames
        // pass through, by design: the snapshot is a point-in-time query, not a
        // tape frame, so a DropNextAccountUpdate divergence does not suppress it.
        //
        // Scope: this runs on CONNECT (and so on reset/reconnect driven by
        // nautilus, which calls connect again), NOT on the transport's own
        // internal reconnect - the lifecycle reattach replays WS commands only.
        // A pushed account update lost to a blackout spanning an internal
        // reconnect therefore stays lost until the next fill-driven snapshot,
        // deliberately; see the reattach comment in lifecycle.rs for why
        // auto-healing it would undo an armed divergence.
        //
        // Failure policy: a 404 means a server predating GET /account; warn and
        // fall back to the legacy reactive path (the account seeds off the first
        // fill, as before this fix). Any OTHER failure against a server that does
        // publish the route is fatal - warn-and-continue there would silently
        // recreate the exact first-fill cache-miss this fix exists to eliminate.
        match fetch_account(&self.http, &self.http_quota, &http_base_url).await {
            Ok(state) => {
                validate_account_snapshot(&state, self.config.account_id)?;
                handle_exec_message(ServerMessage::AccountState(state), &self.exec_context());
                self.await_account_registered().await?;
            }
            Err(FetchAccountError::NotFound) => {
                tracing::warn!("server predates GET /account; account will seed on first fill");
            }
            Err(err) => {
                return Err(anyhow::Error::new(err).context("initial account snapshot"));
            }
        }
        let client_havoc = client_havoc(&self.config.havoc);

        if self.config.transport_profile.orders_over_http() {
            self.connected.store(true, Ordering::Relaxed);
            self.core.set_connected();
            return Ok(());
        }

        let ws_url = join_url(&self.config.ws_url(), "ws");
        let (cmd_tx, cmd_rx) = unbounded_channel::<ExecWsCommand>();
        self.ws_cmd = Some(cmd_tx);

        let connected = Arc::clone(&self.connected);
        let havoc_filter = Arc::new(tokio::sync::Mutex::new(HavocFilter::from_client(
            &client_havoc,
        )));
        // Exec drain pipelines the per-message havoc latency through a pump
        // rather than sleeping inline in the reader loop, which capped throughput
        // and head-of-line-blocked pings/commands (AD4 - see the data client and
        // spawn_latency_pump). The pump owns a clone of the exec context and
        // applies each event to the mirror off-loop. handle_exec_message is
        // already called concurrently from the HTTP order-dispatch tasks, so
        // moving the WS drain's calls onto the pump task adds no new sharing.
        let (deliver_tx, deliver_rx) = unbounded_channel::<HavocDelivery>();
        let pump_ctx = self.exec_context();
        let pump_handle = spawn_latency_pump(deliver_rx, move |msg| {
            let ctx = pump_ctx.clone();
            async move {
                handle_exec_message(msg, &ctx);
            }
        });
        track_task(&self.task_handles, pump_handle);

        let handler_filter = Arc::clone(&havoc_filter);
        let handler_deliver = deliver_tx.clone();
        let disconnect_filter = Arc::clone(&havoc_filter);
        let disconnect_deliver = deliver_tx;
        let task_ws_url = ws_url.clone();
        let reader_handle = tokio::spawn(async move {
            run_ws_connection(
                WsConnectionConfig {
                    ws_url: task_ws_url,
                    conn,
                    seed: client_havoc.seed,
                    connected,
                    sim,
                    label: "exec",
                },
                cmd_rx,
                exec_command_to_client_message,
                Vec::new,
                move |server_msg| {
                    let handler_filter = Arc::clone(&handler_filter);
                    let handler_deliver = handler_deliver.clone();
                    async move {
                        let mut filter = handler_filter.lock().await;
                        enqueue_havoc(&mut filter, server_msg, sim, &handler_deliver);
                    }
                },
                move || {
                    let disconnect_filter = Arc::clone(&disconnect_filter);
                    let disconnect_deliver = disconnect_deliver.clone();
                    async move {
                        let mut filter = disconnect_filter.lock().await;
                        flush_havoc_into_pump(&mut filter, sim, &disconnect_deliver);
                    }
                },
            )
            .await;
        });

        track_task(&self.task_handles, reader_handle);
        // See MogwaiDataClient::connect: a timed-out connect must abort the
        // just-spawned reader and clear the stale handle/ws_cmd so a retry does
        // not orphan the first task racing on the shared `connected` flag.
        if let Err(err) = wait_connected(&self.connected, &ws_url).await {
            if let Some(handle) = lock_recover(&self.task_handles, "task handles").pop() {
                handle.abort();
            }
            self.ws_cmd = None;
            return Err(err);
        }
        self.core.set_connected();
        Ok(())
    }

    async fn disconnect(&mut self) -> anyhow::Result<()> {
        self.stop()
    }

    fn submit_order(&self, cmd: SubmitOrder) -> anyhow::Result<()> {
        let order = self.core.get_order(&cmd.client_order_id)?;

        // Build the wire order FIRST (AE8). An unsupported side/type/TIF errors
        // out of convert::wire_* here; emitting OrderSubmitted before this - as
        // the code used to - queued a Submitted event that nautilus then had to
        // apply to an order it had already denied (Initialized -> Denied) on the
        // very same failed conversion, producing a stray event, a scary
        // invalid-transition log, and (on a later send failure) a permanently
        // Submitted mirror stray that fed the unbounded ExecState growth.
        // Converting first means a conversion failure returns before any event is
        // emitted or any mirror record exists.
        let wire = mogwai_protocol::SubmitOrder {
            client_order_id: cmd.client_order_id.to_string(),
            symbol: symbol_from_instrument(cmd.instrument_id),
            side: convert::wire_side(cmd.order_init.order_side)?,
            order_type: convert::wire_order_type(cmd.order_init.order_type)?,
            quantity: cmd.order_init.quantity.as_decimal(),
            price: cmd.order_init.price.map(|p| p.as_decimal()),
            time_in_force: convert::wire_time_in_force(cmd.order_init.time_in_force)?,
        };

        let ts_init = now_unix_nanos(self.sim);
        let submitted = OrderSubmitted::new(
            self.core.trader_id,
            order.strategy_id(),
            order.instrument_id(),
            order.client_order_id(),
            self.core.account_id,
            UUID4::new(),
            ts_init,
            ts_init,
        );

        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("execution state mutex poisoned"))?;
            state.orders.insert(
                cmd.client_order_id,
                OrderRecord {
                    strategy_id: cmd.strategy_id,
                    instrument_id: cmd.instrument_id,
                    order_side: cmd.order_init.order_side,
                    order_type: cmd.order_init.order_type,
                    status: OrderStatus::Submitted,
                    quantity: cmd.order_init.quantity.as_decimal(),
                    price: cmd.order_init.price.map(|p| p.as_decimal()),
                    filled_qty: Decimal::ZERO,
                    avg_px: None,
                    venue_order_id: None,
                    ts_accepted: cmd.ts_init,
                    ts_last: cmd.ts_init,
                    seen_trades: std::collections::HashSet::new(),
                },
            );
            state.prune();
        }

        // Emit Submitted only now that conversion has succeeded and the mirror
        // record exists. The dispatch below may still fail at transport (WS
        // channel gone, or an HTTP POST error), in which case dispatch_order
        // synthesizes the matching OrderRejected - a valid Submitted -> Rejected
        // transition - so the order still reaches a terminal state.
        self.emitter
            .send_order_event(OrderEventAny::Submitted(submitted));

        self.dispatch_order(ExecWsCommand::Submit(wire))
    }

    fn modify_order(&self, cmd: ModifyOrder) -> anyhow::Result<()> {
        self.dispatch_order(ExecWsCommand::Modify {
            client_order_id: cmd.client_order_id.to_string(),
            price: cmd.price.map(|p| p.as_decimal()),
            quantity: cmd.quantity.map(|q| q.as_decimal()),
        })
    }

    fn cancel_order(&self, cmd: CancelOrder) -> anyhow::Result<()> {
        self.dispatch_order(ExecWsCommand::Cancel {
            client_order_id: cmd.client_order_id.to_string(),
        })
    }

    /// The execution manager's in-flight probe: query the venue's truth for
    /// one order and emit the report if the venue knows it. Follows the
    /// canonical adapter shape (e.g. kraken): spawn the async query, emit via
    /// the report channel, log-and-drop on failure (the manager retries on
    /// its own cadence). A venue that reports no such order emits nothing -
    /// "absent" is the truthful answer for a submit that never reached the
    /// accept gate.
    fn query_order(&self, cmd: QueryOrder) -> anyhow::Result<()> {
        let query = self.venue_query();
        let instruments = Arc::clone(&self.instruments);
        let account_id = self.core.account_id;
        let emitter = self.emitter.clone();
        let sim = self.sim;
        let client_order_id = cmd.client_order_id.to_string();
        track_task(
            &self.task_handles,
            get_runtime().spawn(async move {
                match query
                    .order_status(Some(client_order_id.clone()), false)
                    .await
                {
                    Ok(snapshot) => {
                        let ts_init = now_unix_nanos(sim);
                        let report = snapshot.orders.first().and_then(|info| {
                            order_status_report_from_info(info, &instruments, account_id, ts_init)
                        });
                        match report {
                            Some(report) => emitter.send_order_status_report(report),
                            None => tracing::warn!(
                                order = %client_order_id,
                                "query_order: venue has no record of this order"
                            ),
                        }
                    }
                    Err(err) => tracing::warn!(
                        order = %client_order_id,
                        error = %err,
                        "query_order failed; the manager will retry on its own cadence"
                    ),
                }
            }),
        );
        Ok(())
    }

    /// Order status reports from VENUE TRUTH, not the client-side mirror.
    ///
    /// The mirror is populated by the same lifecycle stream havoc corrupts,
    /// so a report built from it can only repeat the client's (possibly
    /// stale) belief - in the exact fault class reconciliation exists to
    /// catch (a server-side cancel whose event was dropped), a mirror-based
    /// report confidently confirms the stale open order. Querying the venue
    /// over the wire makes this generator a second, independent witness: the
    /// reply content is always a truthful engine book read (honest-content
    /// contract on `ClientMessage::QueryOrders`), while havoc may still
    /// delay or drop its DELIVERY - which surfaces here as a query timeout,
    /// exercising the consumer's own timeout path.
    async fn generate_order_status_reports(
        &self,
        cmd: &GenerateOrderStatusReports,
    ) -> anyhow::Result<Vec<OrderStatusReport>> {
        let snapshot = self.venue_query().order_status(None, cmd.open_only).await?;
        let ts_init = now_unix_nanos(self.sim);
        let reports = snapshot
            .orders
            .iter()
            .filter(|info| {
                cmd.instrument_id
                    .is_none_or(|id| symbol_from_instrument(id) == info.symbol)
            })
            .filter(|info| {
                // An open order requested under open_only is always included,
                // regardless of when it last had activity: a real venue
                // mass-status returns every resting order, and reconciliation
                // passes a lookback-bounded `start`, so filtering a long-quiet
                // open order by `ts_last` used to hide it - and the manager
                // then inferred it canceled-at-venue (AE10). The time filter
                // still applies to closed/historical records (open_only false).
                (cmd.open_only && info.status.is_open())
                    || in_time_range(UnixNanos::from(info.ts_last), cmd.start, cmd.end)
            })
            .filter_map(|info| {
                order_status_report_from_info(
                    info,
                    &self.instruments,
                    self.core.account_id,
                    ts_init,
                )
            })
            .collect();
        Ok(reports)
    }

    /// The singular twin, backing the execution manager's in-flight re-query
    /// (`QueryOrder` probes past the inflight threshold) - previously the
    /// trait's log-and-`None` default, which left mogwai unable to resolve an
    /// in-flight order and forced the consumer's local INFLIGHT_TIMEOUT
    /// reject. Same venue-truth source as the plural generator.
    async fn generate_order_status_report(
        &self,
        cmd: &GenerateOrderStatusReport,
    ) -> anyhow::Result<Option<OrderStatusReport>> {
        let target = cmd.client_order_id.map(|id| id.to_string());
        let snapshot = self.venue_query().order_status(target, false).await?;
        let ts_init = now_unix_nanos(self.sim);
        let report = snapshot
            .orders
            .iter()
            .filter(|info| {
                cmd.venue_order_id
                    .is_none_or(|id| info.venue_order_id == id.as_str())
            })
            .filter(|info| {
                cmd.instrument_id
                    .is_none_or(|id| symbol_from_instrument(id) == info.symbol)
            })
            .find_map(|info| {
                order_status_report_from_info(
                    info,
                    &self.instruments,
                    self.core.account_id,
                    ts_init,
                )
            });
        Ok(report)
    }

    /// Fill reports from VENUE TRUTH (see `generate_order_status_reports`):
    /// the engine books each fill exactly once regardless of how many
    /// `OrderFilled` events the wire carried, so this reply is the ground
    /// truth a duplicated or dropped fill stream reconciles against.
    async fn generate_fill_reports(
        &self,
        cmd: GenerateFillReports,
    ) -> anyhow::Result<Vec<FillReport>> {
        let snapshot = self.venue_query().fill_history(None).await?;
        let ts_init = now_unix_nanos(self.sim);
        let reports = snapshot
            .fills
            .iter()
            .filter(|fill| {
                cmd.instrument_id
                    .is_none_or(|id| symbol_from_instrument(id) == fill.symbol)
            })
            .filter(|fill| {
                cmd.venue_order_id
                    .is_none_or(|id| fill.venue_order_id == id.as_str())
            })
            .filter(|fill| in_time_range(UnixNanos::from(fill.ts_event), cmd.start, cmd.end))
            .filter_map(|fill| {
                fill_report_from_wire(fill, &self.instruments, self.core.account_id, ts_init)
            })
            .collect();
        Ok(reports)
    }

    /// Position status reports from VENUE TRUTH, completing the set alongside
    /// `generate_order_status_reports` and `generate_fill_reports`.
    ///
    /// These used to be rebuilt from the client-side account-snapshot mirror,
    /// which is populated by the same pushed `AccountState` stream havoc
    /// corrupts - and mogwai ships a divergence, `DropNextAccountUpdate`, whose
    /// entire purpose is swallowing one of those pushes. A mirror-built report
    /// therefore confidently CONFIRMS a stale position in precisely the fault
    /// class position reconciliation exists to catch, which is the same
    /// argument that moved the order and fill generators onto the venue-truth
    /// surface.
    ///
    /// `GET /account` is the truthful source and is deliberately not the pushed
    /// frame: it is a point-in-time pull that bypasses the `HavocFilter`, so an
    /// armed `DropNextAccountUpdate` cannot suppress it (see `connect`'s
    /// initial snapshot). The route is transport-agnostic, so this works
    /// unchanged under the HTTP order profiles that never open a `/ws` socket.
    ///
    /// A failed pull propagates rather than falling back to any client-side
    /// belief: an error makes reconciliation fail loudly, whereas a silent
    /// fallback would reintroduce the stale confirmation this exists to remove.
    /// The ONE exception is a 404, which `connect` already treats as a server
    /// predating the route and continues past - failing here would turn that
    /// documented legacy path into a hard failure of the whole mass status,
    /// taking the order and fill reports down with it.
    async fn generate_position_status_reports(
        &self,
        cmd: &GeneratePositionStatusReports,
    ) -> anyhow::Result<Vec<PositionStatusReport>> {
        let state =
            match fetch_account(&self.http, &self.http_quota, &self.config.http_base_url()).await {
                Ok(state) => state,
                Err(FetchAccountError::NotFound) => {
                    tracing::warn!(
                        "server predates GET /account; reporting no positions rather than failing \
                     the whole mass status - position reconciliation is blind against this server"
                    );
                    return Ok(Vec::new());
                }
                Err(err) => {
                    return Err(anyhow::Error::new(err).context("position status venue truth"));
                }
            };
        validate_account_snapshot(&state, self.config.account_id)?;
        // The venue's snapshot instant is the honest `ts_last` for every row:
        // the wire `Position` carries no per-symbol activity timestamp, and
        // dating a report off the client's own last-seen event would put a
        // mirror timestamp on a venue-sourced number.
        let ts_event = UnixNanos::from(state.ts_event);
        let ts_init = now_unix_nanos(self.sim);
        let reports = state
            .positions
            .iter()
            .filter(|position| {
                // Match the WHOLE instrument id, venue included. The wire rows
                // carry only a symbol, so comparing symbols alone would let a
                // request scoped to BTCUSDT on some other venue match this
                // venue's BTCUSDT position - a filter the caller asked for and
                // did not get. Every row here is by construction a MOGWAI one.
                cmd.instrument_id.is_none_or(|id| {
                    id.venue == *MOGWAI_VENUE && symbol_from_instrument(id) == position.symbol
                })
            })
            .filter(|position| {
                // Every position the venue reports is a current OPEN (nonzero)
                // one - the engine removes a symbol from its position map the
                // moment it goes flat - so a lookback-bounded `start` must not
                // hide a long-quiet resting position; reconciliation would
                // otherwise have to re-adopt it as EXTERNAL mid-run (AE10).
                // The time filter therefore only guards a defensive flat row.
                !position.quantity.is_zero() || in_time_range(ts_event, cmd.start, cmd.end)
            })
            .filter_map(|position| {
                let def = instrument_def(&self.instruments, &position.symbol)?;
                let quantity = convert::quantity(position.quantity.abs(), def.size_precision)
                    .map_err(|err| {
                        tracing::warn!(
                            symbol = %position.symbol,
                            error = %err,
                            "dropping position report: unrepresentable quantity"
                        );
                    })
                    .ok()?;
                Some(PositionStatusReport::new(
                    self.core.account_id,
                    convert::instrument_id(&def),
                    position_side(position.quantity),
                    quantity,
                    ts_event,
                    ts_init,
                    None,
                    None,
                    Some(position.avg_px),
                ))
            })
            .collect();
        Ok(reports)
    }

    /// Composes the three report generators into the mass status the live
    /// node's startup reconciliation consumes. The trait default returns
    /// `Ok(None)`, which the node logs as "no mass status available (likely
    /// adapter error)" and then reconciles NOTHING - a worker restarted while
    /// holding an open mogwai position would boot flat and only discover the
    /// venue net via the periodic position poll, mid-run, as a late EXTERNAL
    /// adoption. Following the canonical adapter shape (e.g. kraken spot):
    /// open orders, their fills, and current positions, bounded by the
    /// caller's lookback.
    ///
    /// `lookback_mins` maps to the same `start` bound the three generators
    /// already apply (each filters records by `ts_last`/`ts_event` within
    /// `[start, end]`), computed against sim-now because every mirrored
    /// timestamp lives on the venue's sim axis. `None` means unbounded.
    async fn generate_mass_status(
        &self,
        lookback_mins: Option<u64>,
    ) -> anyhow::Result<Option<ExecutionMassStatus>> {
        let ts_init = now_unix_nanos(self.sim);
        let start = lookback_mins.map(|mins| {
            UnixNanos::from(
                ts_init
                    .as_u64()
                    .saturating_sub(mins.saturating_mul(60 * 1_000_000_000)),
            )
        });
        let order_reports = self
            .generate_order_status_reports(&GenerateOrderStatusReports::new(
                UUID4::new(),
                ts_init,
                true,
                None,
                start,
                None,
                None,
                None,
            ))
            .await?;
        let fill_reports = self
            .generate_fill_reports(GenerateFillReports::new(
                UUID4::new(),
                ts_init,
                None,
                None,
                start,
                None,
                None,
                None,
            ))
            .await?;
        let position_reports = self
            .generate_position_status_reports(&GeneratePositionStatusReports::new(
                UUID4::new(),
                ts_init,
                None,
                start,
                None,
                None,
                None,
            ))
            .await?;

        let mut mass_status = ExecutionMassStatus::new(
            self.core.client_id,
            self.core.account_id,
            *MOGWAI_VENUE,
            ts_init,
            None,
        );
        mass_status.add_order_reports(order_reports);
        mass_status.add_fill_reports(fill_reports);
        mass_status.add_position_reports(position_reports);

        Ok(Some(mass_status))
    }
}

#[derive(Debug, Clone)]
enum ExecWsCommand {
    Submit(mogwai_protocol::SubmitOrder),
    Cancel {
        client_order_id: String,
    },
    Modify {
        client_order_id: String,
        price: Option<Decimal>,
        quantity: Option<Decimal>,
    },
    /// Venue-truth order status query (see `VenueQuery`): sent over the exec
    /// socket, correlated to its reply by `request_id`.
    QueryOrders {
        request_id: String,
        client_order_id: Option<String>,
        open_only: bool,
    },
    /// Venue-truth fill history query, the `QueryOrders` twin.
    QueryFills {
        request_id: String,
        client_order_id: Option<String>,
    },
}

fn exec_command_to_client_message(cmd: ExecWsCommand) -> ClientMessage {
    match cmd {
        ExecWsCommand::Submit(order) => ClientMessage::SubmitOrder(order),
        ExecWsCommand::Cancel { client_order_id } => ClientMessage::CancelOrder { client_order_id },
        ExecWsCommand::Modify {
            client_order_id,
            price,
            quantity,
        } => ClientMessage::ModifyOrder {
            client_order_id,
            price,
            quantity,
        },
        ExecWsCommand::QueryOrders {
            request_id,
            client_order_id,
            open_only,
        } => ClientMessage::QueryOrders {
            request_id,
            client_order_id,
            open_only,
        },
        ExecWsCommand::QueryFills {
            request_id,
            client_order_id,
        } => ClientMessage::QueryFills {
            request_id,
            client_order_id,
        },
    }
}

/// Maps a transport-level failure (the HTTP POST for the command never got a
/// venue reply) onto the `ServerMessage` shape `handle_exec_message` already
/// knows how to turn into a nautilus event. Only valid for `Submit`/`Modify`:
/// a failed `Cancel` is not a full order rejection (the order is still live,
/// or its fate is simply unknown) and is handled by `emit_cancel_rejected`
/// before the call site ever reaches this function - see the comment at the
/// `dispatch_order` call site.
fn reject_for(cmd: &ExecWsCommand, err: &anyhow::Error, sim: SimClock) -> ServerMessage {
    let reason = err.to_string();
    let ts_event = now_unix_nanos(sim).as_u64();
    match cmd {
        ExecWsCommand::Submit(order) => ServerMessage::OrderRejected {
            client_order_id: order.client_order_id.clone(),
            reason,
            ts_event,
        },
        ExecWsCommand::Modify {
            client_order_id, ..
        } => ServerMessage::OrderModifyRejected {
            client_order_id: client_order_id.clone(),
            venue_order_id: None,
            reason,
            ts_event,
        },
        ExecWsCommand::Cancel { client_order_id } => unreachable!(
            "cancel transport failures are reported via emit_cancel_rejected, \
             not reject_for (client_order_id={client_order_id})"
        ),
        ExecWsCommand::QueryOrders { .. } | ExecWsCommand::QueryFills { .. } => unreachable!(
            "queries never pass through dispatch_order; their transport \
             failures surface as errors from VenueQuery itself"
        ),
    }
}

/// Synthesizes the nautilus reject for a command whose transport failed before
/// the venue ever saw it - shared by the HTTP POST error path and the WS
/// send-failure path (AE9). A failed `Cancel` is reported as a `CancelRejected`
/// (the order is still live, or its fate is simply unknown, not dead), leaving
/// the mirrored status untouched; a failed `Submit`/`Modify` is reported as the
/// matching `OrderRejected`/`OrderModifyRejected` so the order reaches a terminal
/// state instead of wedging in `Submitted`/`PendingUpdate`. Both bypass the
/// per-dispatch `HavocFilter` by design: the failure is purely local and never
/// traveled the wire, so there is nothing for the venue-havoc pipeline to model,
/// and routing a terminal reject through a `drop_prob` draw could discard it
/// entirely, leaving nautilus and the mirror stuck forever.
fn synthesize_transport_reject(cmd: &ExecWsCommand, err: &anyhow::Error, ctx: &ExecContext) {
    if let ExecWsCommand::Cancel { client_order_id } = cmd {
        if let Some(client_order_id) = wire_client_order_id(client_order_id) {
            emit_cancel_rejected(
                client_order_id,
                None,
                err.to_string(),
                now_unix_nanos(ctx.sim),
                ctx,
            );
        }
    } else {
        handle_exec_message(reject_for(cmd, err, ctx.sim), ctx);
    }
}

/// Reports a rejected `Cancel` as a nautilus `OrderCancelRejected` without
/// touching the mirrored order's status. Serves both origins:
///
/// - a `Cancel` that failed at TRANSPORT (the HTTP POST never reached the
///   venue): `ts_event` is sim-now and `wire_venue_order_id` is `None`, and
/// - a venue-originated `ServerMessage::OrderCancelRejected` (the engine could
///   not honor the cancel): `ts_event` is the venue's stamp and the wire may
///   name the venue id.
///
/// Unlike a submit rejection, a failed cancel does not mean the order is dead:
/// nautilus's own order FSM restores the pre-cancel status on `CancelRejected`
/// (see `orders/mod.rs`'s `(PendingCancel, CancelRejected) => PendingCancel`
/// transition), so the mirror must likewise leave `record.status` untouched -
/// the order is still whatever it was (Accepted, PartiallyFilled, ...) before
/// the cancel was attempted. The emitted venue id prefers the wire value and
/// falls back to the mirror's, so a wire `None` on a known order still carries
/// the id the adapter already holds.
fn emit_cancel_rejected(
    client_order_id: ClientOrderId,
    wire_venue_order_id: Option<VenueOrderId>,
    reason: String,
    ts_event: UnixNanos,
    ctx: &ExecContext,
) {
    let Some(record) = order_record(&ctx.state, client_order_id) else {
        // Same limitation as OrderRejected/OrderModifyRejected (A.11): the
        // mirror lacks the order, so we have no real strategy_id/
        // instrument_id, and a placeholder would be silently dropped by
        // nautilus `Order.apply` strategy-id validation. Make the drop
        // visible rather than silent.
        tracing::warn!(
            order = %client_order_id,
            reason = %reason,
            "cancel rejected for an order the mirror does not know; \
             reject not surfaced to nautilus (A.11)"
        );
        return;
    };
    let event = OrderCancelRejected::new(
        ctx.trader_id,
        record.strategy_id,
        record.instrument_id,
        client_order_id,
        reason.into(),
        UUID4::new(),
        ts_event,
        now_unix_nanos(ctx.sim),
        false,
        wire_venue_order_id.or(record.venue_order_id),
        Some(ctx.account_id),
    );
    ctx.emitter
        .send_order_event(OrderEventAny::CancelRejected(event));
}

#[derive(Clone)]
struct ExecContext {
    emitter: ExecutionEventEmitter,
    state: Arc<Mutex<ExecState>>,
    instruments: Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    /// Shared with `MogwaiExecutionClient.pending`: the WS reader resolves
    /// venue-truth query waiters here as snapshot replies land.
    pending: Arc<Mutex<PendingQueries>>,
    trader_id: nautilus_model::identifiers::TraderId,
    account_id: AccountId,
    account_type: nautilus_model::enums::AccountType,
    sim: SimClock,
}

#[derive(Debug, Default)]
struct ExecState {
    orders: HashMap<ClientOrderId, OrderRecord>,
    /// `ts_event` of the last account snapshot forwarded to nautilus.
    ///
    /// Snapshots must apply in VENUE order, not arrival order: nautilus applies
    /// account states in arrival order with no staleness guard of its own, so
    /// an older snapshot delivered late by reorder or duplicate havoc would
    /// overwrite newer balances and stay wrong until the next fill-driven
    /// snapshot - which may never come. `handle_account_state` skips any
    /// snapshot below this watermark.
    ///
    /// There is deliberately no position mirror behind this watermark any more.
    /// One existed to serve `generate_position_status_reports`, and it was that
    /// generator's only reader; since the generator now pulls venue truth from
    /// `GET /account`, a client-side copy could only ever be a second, staler
    /// answer to a question the venue already answers - and mogwai's own
    /// `DropNextAccountUpdate` divergence exists to make exactly that copy
    /// wrong.
    account_ts_last: UnixNanos,
}

/// Cap on retained terminal order records. Open orders are never pruned (they
/// are live reconciliation truth); only closed records beyond this many are
/// dropped, oldest-by-`ts_last` first, so a long forward run cannot
/// accumulate terminal orders without bound (AE6). (The mirror once kept an
/// append-only fill Vec with its own cap; fill reports now come from the
/// venue-truth `QueryFills`, so no fill store remains to bound.)
const MAX_TERMINAL_ORDERS: usize = 10_000;

impl ExecState {
    /// Bounds the mirror's memory: an unpruned `orders` map (terminal records
    /// and permanently-Submitted strays live forever) otherwise grows
    /// linearly over a long forward run. Prunes the oldest terminal orders
    /// past the cap; open orders are always retained. Called after each
    /// mirror mutation that can grow the map (a submit insert), and does real
    /// work only when the cap is exceeded.
    fn prune(&mut self) {
        // Cheap length gate before the O(n) terminal scan: the terminal count
        // cannot exceed the cap unless the whole map does, so this keeps prune
        // O(1) on the hot submit/fill paths until the mirror genuinely grows
        // large.
        if self.orders.len() <= MAX_TERMINAL_ORDERS {
            return;
        }
        let terminal = self
            .orders
            .values()
            .filter(|record| record.status.is_closed())
            .count();
        if terminal > MAX_TERMINAL_ORDERS {
            let excess = terminal - MAX_TERMINAL_ORDERS;
            let mut terminal_ids: Vec<(ClientOrderId, UnixNanos)> = self
                .orders
                .iter()
                .filter(|(_, record)| record.status.is_closed())
                .map(|(id, record)| (*id, record.ts_last))
                .collect();
            terminal_ids.sort_by_key(|(_, ts_last)| *ts_last);
            for (id, _) in terminal_ids.into_iter().take(excess) {
                self.orders.remove(&id);
            }
        }
    }
}

#[derive(Debug, Clone)]
struct OrderRecord {
    strategy_id: nautilus_model::identifiers::StrategyId,
    instrument_id: InstrumentId,
    order_side: OrderSide,
    order_type: OrderType,
    status: OrderStatus,
    quantity: Decimal,
    price: Option<Decimal>,
    filled_qty: Decimal,
    avg_px: Option<Decimal>,
    venue_order_id: Option<VenueOrderId>,
    ts_accepted: UnixNanos,
    ts_last: UnixNanos,
    /// `trade_id`s already applied to this order's reconciliation mirror. The
    /// duplicate-fill divergence (`DuplicateNextFill`) and client-side
    /// `duplicate_prob` deliberately deliver the same `OrderFilled` twice; the
    /// duplicate wire event is forwarded downstream (the intended divergence),
    /// but it must not double-apply to the mirror, so the second sighting of a
    /// `trade_id` skips the filled_qty/avg_px/fills mutation.
    seen_trades: std::collections::HashSet<TradeId>,
}

fn handle_exec_message(msg: ServerMessage, ctx: &ExecContext) {
    match msg {
        ServerMessage::OrderAccepted {
            client_order_id,
            venue_order_id,
            ts_event,
        } => {
            let Some(client_order_id) = wire_client_order_id(&client_order_id) else {
                return;
            };
            let Some(venue_order_id) = wire_venue_order_id(&venue_order_id) else {
                return;
            };
            let Some((record, stale)) = with_order_record(&ctx.state, client_order_id, |record| {
                // Terminal-state guard: reorder/duplicate havoc can deliver this
                // Accepted AFTER the fill or cancel that ended the order (the
                // engine emits Accepted and Filled adjacently for immediate
                // fills - exactly the pair a reorder transposes). Nautilus's own
                // order FSM has no terminal-to-Accepted arm, and this mirror is
                // the reconciliation truth source, so it must be at least as
                // strict: never regress a terminal record. The wire event is
                // still forwarded below (the intended divergence, matching the
                // duplicate-fill discipline); only the mirror mutation is
                // skipped.
                let stale = record.status.is_closed();
                if !stale {
                    record.status = OrderStatus::Accepted;
                    record.venue_order_id = Some(venue_order_id);
                    record.ts_accepted = UnixNanos::from(ts_event);
                    // Forward-only, matching the fill handler (F11): a non-terminal
                    // Accepted reordered behind an event that already advanced the
                    // record must not walk ts_last backward and perturb the
                    // in_time_range report filtering.
                    record.ts_last = record.ts_last.max(UnixNanos::from(ts_event));
                }
                (record.clone(), stale)
            }) else {
                // Same A.11 limitation as the reject arms: the mirror lacks the
                // order (e.g. an event arriving after reset() cleared it), so
                // there is no real strategy_id/instrument_id to emit with. Make
                // the drop visible instead of silent.
                tracing::warn!(
                    order = %client_order_id,
                    venue_order_id = %venue_order_id,
                    "order accepted for an order the mirror does not know; \
                     event not surfaced to nautilus (A.11)"
                );
                return;
            };
            if stale {
                tracing::warn!(
                    order = %client_order_id,
                    status = ?record.status,
                    "accepted event for a terminal mirror record; keeping the \
                     terminal status (reordered or duplicated event)"
                );
            }
            let event = OrderAccepted::new(
                ctx.trader_id,
                record.strategy_id,
                record.instrument_id,
                client_order_id,
                venue_order_id,
                ctx.account_id,
                UUID4::new(),
                UnixNanos::from(ts_event),
                now_unix_nanos(ctx.sim),
                false,
            );
            ctx.emitter.send_order_event(OrderEventAny::Accepted(event));
        }
        ServerMessage::OrderRejected {
            client_order_id,
            reason,
            ts_event,
        } => {
            let Some(client_order_id) = wire_client_order_id(&client_order_id) else {
                return;
            };
            let Some(record) = with_order_record(&ctx.state, client_order_id, |record| {
                // Unconditional, unlike the terminal-state guards on the other
                // arms, and deliberately so: the engine emits `Rejected` as an
                // order's SOLE lifecycle event, so no reordered pair can arrive
                // to regress a later terminal state. The one reachable overwrite
                // is the HTTP carrier synthesizing a reject for an order the
                // venue actually processed, which is the known unrecoverable
                // desync (the mirror cannot heal it without a venue-truth query
                // surface) and not something a guard here would fix.
                record.status = OrderStatus::Rejected;
                record.ts_last = UnixNanos::from(ts_event);
                record.clone()
            }) else {
                // The local mirror does not know this order, so we lack the
                // real strategy_id/instrument_id the emit requires. We cannot
                // synthesize them: nautilus `Order.apply` hard-validates the
                // event's strategy_id against the cached order and silently
                // drops the event on mismatch, so a placeholder would guarantee
                // the drop rather than surface the reject. Surfacing it
                // correctly (e.g. resolving the order from the nautilus cache,
                // which ExecContext does not hold) is a design change tracked
                // as bug-hunt A.11; for now make the drop visible instead of
                // silent.
                tracing::warn!(
                    order = %client_order_id,
                    reason = %reason,
                    "order rejected for an order the mirror does not know; \
                     reject not surfaced to nautilus (A.11)"
                );
                return;
            };
            let event = OrderRejected::new(
                ctx.trader_id,
                record.strategy_id,
                record.instrument_id,
                client_order_id,
                ctx.account_id,
                reason.into(),
                UUID4::new(),
                UnixNanos::from(ts_event),
                now_unix_nanos(ctx.sim),
                false,
                false,
            );
            ctx.emitter.send_order_event(OrderEventAny::Rejected(event));
        }
        ServerMessage::OrderCanceled {
            client_order_id,
            venue_order_id,
            ts_event,
        } => {
            let Some(client_order_id) = wire_client_order_id(&client_order_id) else {
                return;
            };
            let Some(venue_order_id) = wire_venue_order_id(&venue_order_id) else {
                return;
            };
            let Some((record, stale)) = with_order_record(&ctx.state, client_order_id, |record| {
                // Terminal-state guard, same as the OrderAccepted arm: a
                // Canceled transposed behind the fill that actually ended the
                // order must not overwrite Filled (or a duplicate re-cancel an
                // already-terminal record). Forward the wire event, skip the
                // mirror regression.
                let stale = record.status.is_closed();
                if !stale {
                    record.status = OrderStatus::Canceled;
                    record.venue_order_id = Some(venue_order_id);
                    // Forward-only, matching the fill handler (F11).
                    record.ts_last = record.ts_last.max(UnixNanos::from(ts_event));
                }
                (record.clone(), stale)
            }) else {
                // Same A.11 limitation as the reject arms: no real strategy_id/
                // instrument_id to emit with. Make the drop visible.
                tracing::warn!(
                    order = %client_order_id,
                    venue_order_id = %venue_order_id,
                    "order canceled for an order the mirror does not know; \
                     event not surfaced to nautilus (A.11)"
                );
                return;
            };
            if stale {
                tracing::warn!(
                    order = %client_order_id,
                    status = ?record.status,
                    "canceled event for a terminal mirror record; keeping the \
                     terminal status (reordered or duplicated event)"
                );
            }
            let event = OrderCanceled::new(
                ctx.trader_id,
                record.strategy_id,
                record.instrument_id,
                client_order_id,
                UUID4::new(),
                UnixNanos::from(ts_event),
                now_unix_nanos(ctx.sim),
                false,
                Some(venue_order_id),
                Some(ctx.account_id),
            );
            ctx.emitter.send_order_event(OrderEventAny::Canceled(event));
        }
        ServerMessage::OrderUpdated {
            client_order_id,
            venue_order_id,
            quantity,
            price,
            leaves_qty,
            ts_event,
        } => {
            let Some(client_order_id) = wire_client_order_id(&client_order_id) else {
                return;
            };
            let Some(venue_order_id) = wire_venue_order_id(&venue_order_id) else {
                return;
            };
            let Some(known) = order_record(&ctx.state, client_order_id) else {
                // Same A.11 limitation as the reject arms: no real strategy_id/
                // instrument_id to emit with. Make the drop visible.
                tracing::warn!(
                    order = %client_order_id,
                    venue_order_id = %venue_order_id,
                    "order update for an order the mirror does not know; \
                     event not surfaced to nautilus (A.11)"
                );
                return;
            };
            // Resolve the instrument before touching the mirror so a missing def
            // does not leave the mirror amended with no matching event emitted.
            let Some(def) = instrument_def(
                &ctx.instruments,
                &symbol_from_instrument(known.instrument_id),
            ) else {
                tracing::warn!(
                    order = %client_order_id,
                    instrument = %known.instrument_id,
                    "dropping order update: no instrument def (cache not seeded?)"
                );
                return;
            };
            // Convert the amend's quantity/price before touching the mirror,
            // same as the missing-def guard above: a hostile amend value that
            // cannot represent as nautilus Price/Quantity must not leave the
            // mirror amended with no event emitted, and must not panic the
            // exec task. Drop the amend with a warning instead.
            let updated_quantity = match convert::quantity(quantity, def.size_precision) {
                Ok(quantity) => quantity,
                Err(err) => {
                    tracing::warn!(
                        order = %client_order_id,
                        error = %err,
                        "dropping order update: unrepresentable quantity"
                    );
                    return;
                }
            };
            let updated_price = match price
                .map(|p| convert::price(p, def.price_precision))
                .transpose()
            {
                Ok(price) => price,
                Err(err) => {
                    tracing::warn!(
                        order = %client_order_id,
                        error = %err,
                        "dropping order update: unrepresentable price"
                    );
                    return;
                }
            };
            let Some((record, stale)) = with_order_record(&ctx.state, client_order_id, |record| {
                // Terminal-state guard, same as the OrderAccepted arm: an amend
                // ack reordered behind the order's terminal event must not
                // recompute a non-terminal status from leaves_qty (or rewrite
                // quantity/price on a record the venue has already closed).
                // Forward the wire event, skip the mirror mutation.
                let stale = record.status.is_closed();
                if !stale {
                    record.venue_order_id = Some(venue_order_id);
                    record.quantity = quantity;
                    record.price = price;
                    // The venue's `leaves_qty` is authoritative for the remaining
                    // size after the amend. Reconcile `filled_qty` so the mirror
                    // invariant `quantity - filled_qty == leaves_qty` holds even
                    // after a downsizing amend (which the bare `quantity` overwrite
                    // used to leave stale, drifting from the venue). Clamp at zero
                    // so a `leaves_qty` exceeding the new total cannot push
                    // `filled_qty` negative.
                    record.filled_qty = (quantity - leaves_qty).max(Decimal::ZERO);
                    // An amend never reverses an in-progress fill: an order with a
                    // non-zero filled_qty stays PARTIALLY_FILLED (or flips to FILLED
                    // when the amend leaves nothing outstanding) so the mirror does
                    // not report Accepted alongside a non-zero filled_qty.
                    record.status = if record.filled_qty.is_zero() {
                        OrderStatus::Accepted
                    } else if leaves_qty.is_zero() {
                        OrderStatus::Filled
                    } else {
                        OrderStatus::PartiallyFilled
                    };
                    // Forward-only, matching the fill handler (F11): a non-terminal
                    // amend ack reordered behind a later event must not walk
                    // ts_last backward.
                    record.ts_last = record.ts_last.max(UnixNanos::from(ts_event));
                }
                (record.clone(), stale)
            }) else {
                return;
            };
            if stale {
                tracing::warn!(
                    order = %client_order_id,
                    status = ?record.status,
                    "update event for a terminal mirror record; keeping the \
                     terminal status (reordered or duplicated event)"
                );
            }
            let event = OrderUpdated::new(
                ctx.trader_id,
                record.strategy_id,
                record.instrument_id,
                client_order_id,
                updated_quantity,
                UUID4::new(),
                UnixNanos::from(ts_event),
                now_unix_nanos(ctx.sim),
                false,
                Some(venue_order_id),
                Some(ctx.account_id),
                updated_price,
                None,
                None,
                false,
            );
            ctx.emitter.send_order_event(OrderEventAny::Updated(event));
        }
        ServerMessage::OrderModifyRejected {
            client_order_id,
            venue_order_id,
            reason,
            ts_event,
        } => {
            let Some(client_order_id) = wire_client_order_id(&client_order_id) else {
                return;
            };
            let Some(record) = order_record(&ctx.state, client_order_id) else {
                // Same limitation as OrderRejected (A.11): the mirror lacks the
                // order, so we have no real strategy_id/instrument_id, and a
                // placeholder would be silently dropped by nautilus
                // `Order.apply` strategy-id validation. The `venue_order_id:
                // None` case (protocol's explicit "id unknown to venue") is
                // exactly when surfacing this matters most, but doing so
                // correctly needs a non-mirror order source (a design change).
                // Make the drop visible rather than silent.
                tracing::warn!(
                    order = %client_order_id,
                    venue_order_id = ?venue_order_id,
                    reason = %reason,
                    "modify rejected for an order the mirror does not know; \
                     reject not surfaced to nautilus (A.11)"
                );
                return;
            };
            let event = OrderModifyRejected::new(
                ctx.trader_id,
                record.strategy_id,
                record.instrument_id,
                client_order_id,
                reason.into(),
                UUID4::new(),
                UnixNanos::from(ts_event),
                now_unix_nanos(ctx.sim),
                false,
                // Prefer the wire id and fall back to the mirror's, matching
                // emit_cancel_rejected: the engine omits the venue id for an
                // order that has gone terminal even though the id is known, so
                // a known order's modify-reject would otherwise reach nautilus
                // with no venue id where the equivalent cancel-reject carries
                // one. The fallback also covers any future wire omission.
                venue_order_id
                    .as_deref()
                    .and_then(wire_venue_order_id)
                    .or(record.venue_order_id),
                Some(ctx.account_id),
            );
            ctx.emitter
                .send_order_event(OrderEventAny::ModifyRejected(event));
        }
        ServerMessage::OrderCancelRejected {
            client_order_id,
            venue_order_id,
            reason,
            ts_event,
        } => {
            // A venue-originated cancel rejection. emit_cancel_rejected leaves
            // the mirror's status untouched (nautilus restores the pre-cancel
            // status) and handles the unknown-order (A.11) drop, exactly as the
            // transport-failure path does - the only differences are the wire
            // timestamp and the possibly-named venue id, both passed through.
            let Some(client_order_id) = wire_client_order_id(&client_order_id) else {
                return;
            };
            emit_cancel_rejected(
                client_order_id,
                venue_order_id.as_deref().and_then(wire_venue_order_id),
                reason,
                UnixNanos::from(ts_event),
                ctx,
            );
        }
        ServerMessage::OrderFilled(fill) => handle_order_filled(&fill, ctx),
        // Venue-truth query replies: resolve the waiter registered under the
        // echoed correlation id. A reply with no waiter is a straggler whose
        // requester already timed out (client havoc delayed it past the
        // request timeout) or a duplicate - log it and move on; the content
        // was truthful either way, only the delivery was havoc'd.
        ServerMessage::OrderStatusSnapshot(snapshot) => {
            let waiter = lock_recover(&ctx.pending, "pending queries")
                .orders
                .remove(&snapshot.request_id);
            match waiter {
                Some(reply_tx) => {
                    if let Err(snapshot) = reply_tx.send(snapshot) {
                        tracing::warn!(
                            request_id = %snapshot.request_id,
                            "order status reply arrived after its requester gave up"
                        );
                    }
                }
                None => tracing::warn!(
                    request_id = %snapshot.request_id,
                    "unsolicited order status snapshot (timed-out or duplicate reply)"
                ),
            }
        }
        ServerMessage::FillSnapshot(snapshot) => {
            let waiter = lock_recover(&ctx.pending, "pending queries")
                .fills
                .remove(&snapshot.request_id);
            match waiter {
                Some(reply_tx) => {
                    if let Err(snapshot) = reply_tx.send(snapshot) {
                        tracing::warn!(
                            request_id = %snapshot.request_id,
                            "fill snapshot reply arrived after its requester gave up"
                        );
                    }
                }
                None => tracing::warn!(
                    request_id = %snapshot.request_id,
                    "unsolicited fill snapshot (timed-out or duplicate reply)"
                ),
            }
        }
        ServerMessage::AccountState(state) => handle_account_state(&state, ctx),
        ServerMessage::Heartbeat { .. } => {
            tracing::trace!("ignoring server heartbeat on execution path");
        }
        ServerMessage::ProtocolError { reason, .. } => {
            // Untargeted (no client_order_id to attribute it to, unlike
            // OrderRejected), so there is no nautilus order event to raise -
            // just make the venue-side decode failure visible in the adapter's
            // own logs.
            tracing::warn!(%reason, "venue reported a protocol error");
        }
        ServerMessage::AdmissionRejected {
            subject,
            reason,
            ts_event,
        } => match subject {
            mogwai_protocol::AdmissionSubject::Submit { client_order_id } => {
                handle_exec_message(
                    ServerMessage::OrderRejected {
                        client_order_id,
                        reason,
                        ts_event,
                    },
                    ctx,
                );
            }
            mogwai_protocol::AdmissionSubject::Cancel { client_order_id } => {
                handle_exec_message(
                    ServerMessage::OrderCancelRejected {
                        client_order_id,
                        venue_order_id: None,
                        reason,
                        ts_event,
                    },
                    ctx,
                );
            }
            mogwai_protocol::AdmissionSubject::Modify { client_order_id } => {
                handle_exec_message(
                    ServerMessage::OrderModifyRejected {
                        client_order_id,
                        venue_order_id: None,
                        reason,
                        ts_event,
                    },
                    ctx,
                );
            }
            mogwai_protocol::AdmissionSubject::Query { request_id, query } => {
                // DROP the waiter rather than answer it. An empty snapshot
                // would be a false venue truth - "you have no orders" when the
                // venue in fact never looked - and the mirror would reconcile
                // against it. Dropping the oneshot sender wakes the requester
                // with a RecvError immediately, exactly as a disconnect does,
                // so it fails fast instead of waiting out its query timeout for
                // a reply the venue has said it will never send.
                //
                // The `query` discriminator is what makes this safe: the two
                // waiter maps are separate and the protocol nowhere requires
                // request ids to be unique across them, so probing both could
                // wake the wrong waiter on a collision.
                let mut pending = lock_recover(&ctx.pending, "pending queries");
                let woken = match query {
                    mogwai_protocol::QueryKind::Orders => {
                        pending.orders.remove(&request_id).is_some()
                    }
                    mogwai_protocol::QueryKind::Fills => {
                        pending.fills.remove(&request_id).is_some()
                    }
                };
                drop(pending);
                tracing::warn!(
                    %request_id,
                    ?query,
                    %reason,
                    woken,
                    "venue refused a venue-truth query; failing its waiter now"
                );
            }
            mogwai_protocol::AdmissionSubject::Frame => {
                tracing::warn!(?subject, %reason, "venue refused request admission");
            }
        },
        // Subscription diagnostics are handled by the data client.
        ServerMessage::Trade(_)
        | ServerMessage::Quote(_)
        | ServerMessage::SubscriptionIssues { .. } => {}
    }
}

fn handle_order_filled(fill: &mogwai_protocol::OrderFilled, ctx: &ExecContext) {
    let Some(client_order_id) = wire_client_order_id(&fill.client_order_id) else {
        return;
    };
    let Some(def) = instrument_def(&ctx.instruments, &fill.symbol) else {
        // A missing instrument def here is a legitimate miss (the instrument
        // cache has not been seeded yet), not a divergence: dropping a real
        // fill silently strands the order in Submitted/Accepted forever, so
        // surface it. (The DropNextAccountUpdate divergence drops account
        // snapshots, not fills, so warning here does not mask it.)
        tracing::warn!(
            symbol = %fill.symbol,
            order = %client_order_id,
            trade = %fill.trade_id,
            "dropping order fill: no instrument def (cache not seeded?)"
        );
        return;
    };
    let Ok(quote_currency) = Currency::from_str(&def.quote) else {
        tracing::warn!(
            symbol = %fill.symbol,
            quote = %def.quote,
            order = %client_order_id,
            "dropping order fill: unknown quote currency"
        );
        return;
    };
    let Some(venue_order_id) = wire_venue_order_id(&fill.venue_order_id) else {
        return;
    };
    // Nautilus caps a `TradeId` at 36 non-empty ASCII chars and the panicking
    // constructors (`TradeId::new`, the `From` impls) assert past that. The
    // engine's ids are short, but this is a wire value: a server bug or havoc
    // corruption sending an over-long/empty/non-ASCII id must drop the fill
    // with a warning rather than panic the unsupervised exec task (the same
    // discipline convert.rs applies to the data-side synthetic trade ids).
    let trade_id = match TradeId::new_checked(&fill.trade_id) {
        Ok(trade_id) => trade_id,
        Err(err) => {
            tracing::warn!(
                order = %client_order_id,
                trade = %fill.trade_id,
                error = %err,
                "dropping order fill: unrepresentable trade id"
            );
            return;
        }
    };
    // Convert the wire price/qty before mutating the mirror so a hostile fill
    // value that overflows nautilus Price/Quantity drops the whole fill with a
    // warning rather than panicking the exec task or leaving the mirror
    // advanced with no event emitted.
    let last_qty = match convert::quantity(fill.last_qty, def.size_precision) {
        Ok(last_qty) => last_qty,
        Err(err) => {
            tracing::warn!(
                order = %client_order_id,
                trade = %trade_id,
                error = %err,
                "dropping order fill: unrepresentable last_qty"
            );
            return;
        }
    };
    let last_px = match convert::price(fill.last_px, def.price_precision) {
        Ok(last_px) => last_px,
        Err(err) => {
            tracing::warn!(
                order = %client_order_id,
                trade = %trade_id,
                error = %err,
                "dropping order fill: unrepresentable last_px"
            );
            return;
        }
    };
    // A repeated `trade_id` for this order is a duplicate fill: the duplicate
    // OrderFilled wire event is still forwarded below (the intended divergence),
    // but the reconciliation mirror (filled_qty/avg_px/fills) must apply each
    // economic fill exactly once, so the second sighting skips the mutation.
    // The duplicate flag guards only the mirror mutation inside the closure;
    // the wire event is forwarded either way (the intended divergence), and
    // fill REPORTS now come from the venue-truth QueryFills rather than any
    // mirror fill store, so nothing outside the closure branches on it.
    let Some((record, _is_duplicate)) = with_order_record(&ctx.state, client_order_id, |record| {
        let is_duplicate = !record.seen_trades.insert(trade_id);
        if !is_duplicate {
            // Terminal-state guard (see the OrderAccepted arm): a partial fill
            // transposed behind the cancel (or the final fill) that ended the
            // order must still book its economics - money moved at the venue,
            // so filled_qty/avg_px/the fill record are real - but must not
            // regress the terminal status back to PartiallyFilled and re-open
            // a closed order in the reconciliation mirror.
            if !record.status.is_closed() {
                record.status = if fill.leaves_qty.is_zero() {
                    OrderStatus::Filled
                } else {
                    OrderStatus::PartiallyFilled
                };
            }
            record.venue_order_id = Some(venue_order_id);
            let previous_notional = record
                .avg_px
                .unwrap_or(Decimal::ZERO)
                .checked_mul(record.filled_qty)
                .unwrap_or(Decimal::ZERO);
            record.filled_qty += fill.last_qty;
            let total_notional = previous_notional
                .checked_add(
                    fill.last_px
                        .checked_mul(fill.last_qty)
                        .unwrap_or(Decimal::ZERO),
                )
                .unwrap_or(previous_notional);
            if !record.filled_qty.is_zero() {
                record.avg_px = total_notional.checked_div(record.filled_qty);
            }
            // A reordered fill carries an OLDER ts_event than the event that
            // already advanced the record; only ever move ts_last forward so
            // the mirror's in_time_range filtering does not walk backward.
            record.ts_last = record.ts_last.max(UnixNanos::from(fill.ts_event));
        }
        (record.clone(), is_duplicate)
    }) else {
        // The worst silent drop on this path: money moved at the venue and no
        // nautilus event can be built (same A.11 limitation as the reject
        // arms - the mirror lacks the order, so there is no real strategy_id/
        // instrument_id to emit with). Make it loud.
        tracing::warn!(
            order = %client_order_id,
            trade = %trade_id,
            venue_order_id = %venue_order_id,
            "order fill for an order the mirror does not know; \
             fill not surfaced to nautilus (A.11)"
        );
        return;
    };

    let commission = if fill.commission.is_zero() {
        None
    } else {
        // The mirror is already advanced and the fill event must still emit; a
        // commission that cannot represent as nautilus Money degrades to no
        // reported commission (with a warning) rather than panicking or
        // dropping the whole fill.
        match convert::money(fill.commission, quote_currency) {
            Ok(money) => Some(money),
            Err(err) => {
                tracing::warn!(
                    order = %client_order_id,
                    trade = %trade_id,
                    error = %err,
                    "reporting fill without commission: unrepresentable amount"
                );
                None
            }
        }
    };
    // mogwai does not report a liquidity side on the wire. The engine fills
    // immediately against replayed history, which is taker-equivalent, so every
    // fill is reported as Taker. This is a deliberate, lossy mapping: the wire
    // carries no maker/taker flag to preserve.
    let event = OrderFilled::new(
        ctx.trader_id,
        record.strategy_id,
        record.instrument_id,
        client_order_id,
        venue_order_id,
        ctx.account_id,
        trade_id,
        record.order_side,
        record.order_type,
        last_qty,
        last_px,
        quote_currency,
        LiquiditySide::Taker,
        UUID4::new(),
        UnixNanos::from(fill.ts_event),
        now_unix_nanos(ctx.sim),
        false,
        None,
        commission,
        None,
    );
    ctx.emitter.send_order_event(OrderEventAny::Filled(event));
}

fn handle_account_state(state: &mogwai_protocol::AccountState, ctx: &ExecContext) {
    // The pushed path is the one that would contaminate SILENTLY: below, the
    // CONFIGURED id is stamped onto the nautilus event regardless of what the
    // wire said, so a misrouted snapshot would be adopted and relabelled as
    // one's own. Compared borrowed, not through `to_string`: this runs on every
    // account frame of every fill.
    if state.account_id.as_str() != ctx.account_id.as_ref() {
        tracing::error!(
            wire_account = %state.account_id.as_str(),
            expected_account = %ctx.account_id,
            "dropping account snapshot routed to a different account"
        );
        return;
    }
    let ts_event = UnixNanos::from(state.ts_event);
    let balances = state
        .balances
        .iter()
        .filter_map(|balance| {
            // Warn on a balance whose currency string nautilus cannot represent,
            // matching every other unrepresentable-amount drop in this closure
            // (AE18): a bare `.ok()?` swallowed the whole currency's balance with
            // no diagnostic, so an account snapshot silently under-reported.
            let currency = match Currency::from_str(&balance.currency) {
                Ok(currency) => currency,
                Err(err) => {
                    tracing::warn!(
                        currency = %balance.currency,
                        error = %err,
                        "dropping account balance: currency unknown to nautilus"
                    );
                    return None;
                }
            };
            // A hostile balance amount that cannot represent as nautilus Money
            // drops just this currency's balance with a warning rather than
            // panicking the exec task that books the account snapshot.
            let convert_amount = |amount, label| {
                convert::money(amount, currency)
                    .map_err(|err| {
                        tracing::warn!(
                            currency = %balance.currency,
                            field = label,
                            error = %err,
                            "dropping account balance: unrepresentable amount"
                        );
                    })
                    .ok()
            };
            let total = convert_amount(balance.total, "total")?;
            let locked = convert_amount(balance.locked, "locked")?;
            let free = convert_amount(balance.free, "free")?;
            // `AccountBalance::new` hard-asserts `locked + free == total` and
            // panics otherwise. A havoc'd/messy AccountState can carry
            // inconsistent amounts, and even a self-consistent wire decimal
            // snapshot can fail the fixed-point check after each amount is
            // independently rounded to currency precision above. Route
            // through new_checked and drop just this currency's balance
            // (matching convert_amount's own drop-with-warning discipline)
            // rather than panicking the unsupervised exec task.
            match AccountBalance::new_checked(total, locked, free) {
                Ok(balance) => Some(balance),
                Err(err) => {
                    tracing::warn!(
                        currency = %balance.currency,
                        error = %err,
                        "dropping account balance: locked + free != total"
                    );
                    None
                }
            }
        })
        .collect();

    {
        let mut mirror = lock_recover(&ctx.state, "execution state");
        // Snapshots must forward in venue order, not arrival order: nautilus
        // applies account states in arrival order with no staleness guard of
        // its own, so an OLDER snapshot delivered late by reorder/duplicate
        // havoc would overwrite newer balances and stay wrong until the next
        // fill-driven snapshot, which may be never. Skip any snapshot below the
        // applied watermark. Equal-ts duplicates pass; they re-apply
        // idempotently.
        if ts_event < mirror.account_ts_last {
            tracing::warn!(
                ts_event = ts_event.as_u64(),
                last_applied = mirror.account_ts_last.as_u64(),
                "dropping stale account snapshot: older than the last applied one"
            );
            return;
        }
        mirror.account_ts_last = ts_event;
    }
    ctx.emitter.send_account_state(NautilusAccountState::new(
        ctx.account_id,
        ctx.account_type,
        balances,
        Vec::new(),
        true,
        UUID4::new(),
        ts_event,
        now_unix_nanos(ctx.sim),
        None,
    ));
}

/// Converts a server-sent `client_order_id` string into a nautilus
/// `ClientOrderId`, dropping the event with a warning instead of panicking.
/// `ClientOrderId::from` routes through the panicking `new`, which nautilus's
/// `check_valid_string_ascii` rejects on an empty, whitespace-only, or
/// non-ASCII string (there is no length cap, unlike `TradeId`). These are wire
/// values, so a server bug or havoc corruption sending a malformed id must not
/// panic the unsupervised exec task - the same drop-and-warn discipline the
/// fill's `trade_id` already gets.
fn wire_client_order_id(raw: &str) -> Option<ClientOrderId> {
    ClientOrderId::new_checked(raw)
        .map_err(|err| {
            tracing::warn!(
                order = %raw,
                error = %err,
                "dropping exec event: unrepresentable client order id"
            );
        })
        .ok()
}

/// Converts a server-sent `venue_order_id` string into a nautilus
/// `VenueOrderId`, dropping it with a warning instead of panicking. Same
/// rationale as `wire_client_order_id`: `VenueOrderId::from` panics on empty,
/// whitespace-only, or non-ASCII wire strings.
fn wire_venue_order_id(raw: &str) -> Option<VenueOrderId> {
    VenueOrderId::new_checked(raw)
        .map_err(|err| {
            tracing::warn!(
                venue_order_id = %raw,
                error = %err,
                "dropping exec event: unrepresentable venue order id"
            );
        })
        .ok()
}

fn with_order_record<T>(
    state: &Arc<Mutex<ExecState>>,
    client_order_id: ClientOrderId,
    f: impl FnOnce(&mut OrderRecord) -> T,
) -> Option<T> {
    lock_recover(state, "execution state")
        .orders
        .get_mut(&client_order_id)
        .map(f)
}

fn order_record(
    state: &Arc<Mutex<ExecState>>,
    client_order_id: ClientOrderId,
) -> Option<OrderRecord> {
    lock_recover(state, "execution state")
        .orders
        .get(&client_order_id)
        .cloned()
}

fn position_side(quantity: Decimal) -> PositionSideSpecified {
    if quantity.is_sign_positive() && !quantity.is_zero() {
        PositionSideSpecified::Long
    } else if quantity.is_sign_negative() {
        PositionSideSpecified::Short
    } else {
        PositionSideSpecified::Flat
    }
}

fn in_time_range(ts: UnixNanos, start: Option<UnixNanos>, end: Option<UnixNanos>) -> bool {
    start.is_none_or(|start| ts >= start) && end.is_none_or(|end| ts <= end)
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc, time::Duration};

    use mogwai_protocol::{
        ClientHavoc, HavocLatency, Side, TradeTick, TransportProfile, WireOrderStatus,
    };
    use nautilus_common::{cache::Cache, clients::ExecutionClient, messages::ExecutionEvent};
    use nautilus_live::{ExecutionClientCore, ExecutionEventEmitter};
    use nautilus_model::identifiers::{ClientId, StrategyId, TraderId};

    use super::*;

    fn execution_client() -> MogwaiExecutionClient {
        execution_client_with_config(MogwaiExecClientConfig::default())
    }

    fn execution_client_with_config(config: MogwaiExecClientConfig) -> MogwaiExecutionClient {
        let cache = Rc::new(RefCell::new(Cache::default()));
        let core = ExecutionClientCore::new(
            TraderId::from("MOGWAI-001"),
            ClientId::from("MOGWAI-TEST"),
            *MOGWAI_VENUE,
            config.oms_type,
            config.account_id,
            config.account_type,
            None,
            cache,
        );

        MogwaiExecutionClient::new(core, config).expect("valid execution client")
    }

    fn instrument_id() -> InstrumentId {
        InstrumentId::new(
            nautilus_model::identifiers::Symbol::from("BTCUSDT"),
            *MOGWAI_VENUE,
        )
    }

    fn def() -> InstrumentDef {
        InstrumentDef {
            symbol: "BTCUSDT".into(),
            base: "BTC".into(),
            quote: "USDT".into(),
            price_precision: 2,
            size_precision: 8,
            price_increment: Decimal::new(1, 2),
            size_increment: Decimal::new(1, 8),
        }
    }

    fn instruments_map() -> Arc<Mutex<HashMap<Symbol, InstrumentDef>>> {
        Arc::new(Mutex::new(HashMap::from([("BTCUSDT".to_string(), def())])))
    }

    /// Serves `GET /account` from a canned snapshot on an ephemeral loopback
    /// port and points the client's `base_url` at it.
    ///
    /// Position reports come from venue truth over HTTP, not from any
    /// client-side state, so a test that wants the venue to hold a position has
    /// to say so HERE - there is nothing left to seed on the client. Answers
    /// every request with the same body (the tests only ever fetch `/account`),
    /// one connection at a time, for as long as the returned handle lives.
    async fn install_account_venue(
        client: &mut MogwaiExecutionClient,
        positions: Vec<mogwai_protocol::Position>,
        ts_event: u64,
    ) -> tokio::task::JoinHandle<()> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind account venue");
        let port = listener.local_addr().expect("account venue addr").port();
        client.config.base_url = format!("ws://127.0.0.1:{port}");
        let body = serde_json::to_string(&mogwai_protocol::AccountState {
            account_id: mogwai_protocol::AccountId::parse("MOGWAI-001").unwrap(),
            balances: Vec::new(),
            positions,
            ts_event,
        })
        .expect("encode account snapshot");
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let body = body.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    // Read and discard the request head; the body is fixed.
                    let mut buf = [0_u8; 1024];
                    drop(stream.read(&mut buf).await);
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    drop(stream.write_all(response.as_bytes()).await);
                    drop(stream.flush().await);
                });
            }
        })
    }

    fn wire_position(quantity: Decimal) -> mogwai_protocol::Position {
        mogwai_protocol::Position {
            symbol: "BTCUSDT".into(),
            quantity,
            avg_px: Decimal::new(10_000, 2),
        }
    }

    fn seed_order(state: &Arc<Mutex<ExecState>>) {
        state.lock().expect("execution state mutex").orders.insert(
            ClientOrderId::from("O-1"),
            OrderRecord {
                strategy_id: StrategyId::from("S-001"),
                instrument_id: instrument_id(),
                order_side: OrderSide::Buy,
                order_type: OrderType::Limit,
                status: OrderStatus::Submitted,
                quantity: Decimal::new(1, 0),
                price: Some(Decimal::new(10_000, 2)),
                filled_qty: Decimal::ZERO,
                avg_px: None,
                venue_order_id: None,
                ts_accepted: UnixNanos::from(1),
                ts_last: UnixNanos::from(1),
                seen_trades: std::collections::HashSet::new(),
            },
        );
    }

    fn exec_context() -> (
        ExecContext,
        tokio::sync::mpsc::UnboundedReceiver<ExecutionEvent>,
    ) {
        let (tx, rx) = unbounded_channel();
        let config = MogwaiExecClientConfig::default();
        let mut emitter = ExecutionEventEmitter::new(
            get_atomic_clock_realtime(),
            config.trader_id,
            config.account_id,
            config.account_type,
            None,
        );
        emitter.set_sender(tx);
        let state = Arc::new(Mutex::new(ExecState::default()));
        seed_order(&state);
        (
            ExecContext {
                emitter,
                state,
                instruments: instruments_map(),
                pending: Arc::new(Mutex::new(PendingQueries::default())),
                trader_id: config.trader_id,
                account_id: config.account_id,
                account_type: config.account_type,
                sim: SimClock::identity(),
            },
            rx,
        )
    }

    #[test]
    fn account_snapshot_with_a_foreign_account_id_is_an_error() {
        let state = mogwai_protocol::AccountState {
            account_id: mogwai_protocol::AccountId::parse("FOREIGN").unwrap(),
            balances: Vec::new(),
            positions: Vec::new(),
            ts_event: 1,
        };
        let err = validate_account_snapshot(&state, AccountId::from("MOGWAI-001"))
            .expect_err("a snapshot for another account must not be adopted");
        assert!(err.to_string().contains("FOREIGN"));
    }

    #[test]
    fn pushed_account_state_with_a_foreign_account_id_is_rejected() {
        let (ctx, mut events) = exec_context();
        handle_exec_message(
            ServerMessage::AccountState(mogwai_protocol::AccountState {
                account_id: mogwai_protocol::AccountId::parse("FOREIGN").unwrap(),
                balances: Vec::new(),
                positions: Vec::new(),
                ts_event: 1,
            }),
            &ctx,
        );
        assert!(
            events.try_recv().is_err(),
            "foreign account state must not reach nautilus"
        );
    }

    #[test]
    fn admission_rejected_translates_per_command() {
        let (ctx, mut events) = exec_context();
        for subject in [
            mogwai_protocol::AdmissionSubject::Submit {
                client_order_id: "O-1".into(),
            },
            mogwai_protocol::AdmissionSubject::Cancel {
                client_order_id: "O-1".into(),
            },
            mogwai_protocol::AdmissionSubject::Modify {
                client_order_id: "O-1".into(),
            },
        ] {
            handle_exec_message(
                ServerMessage::AdmissionRejected {
                    subject,
                    reason: "admission budget exhausted".into(),
                    ts_event: 2,
                },
                &ctx,
            );
        }
        assert!(
            events.try_recv().is_ok(),
            "submit refusal raises an order rejection"
        );
        assert!(
            events.try_recv().is_ok(),
            "cancel refusal raises a cancel rejection"
        );
        assert!(
            events.try_recv().is_ok(),
            "modify refusal raises a modify rejection"
        );

        use tokio::sync::oneshot;
        let (orders_tx, mut orders_rx) = oneshot::channel();
        let (fills_tx, mut fills_rx) = oneshot::channel();
        {
            let mut pending = ctx.pending.lock().expect("pending queries mutex");
            pending.orders.insert("same-id".into(), orders_tx);
            pending.fills.insert("same-id".into(), fills_tx);
        }
        handle_exec_message(
            ServerMessage::AdmissionRejected {
                subject: mogwai_protocol::AdmissionSubject::Query {
                    request_id: "same-id".into(),
                    query: mogwai_protocol::QueryKind::Orders,
                },
                reason: "admission budget exhausted".into(),
                ts_event: 3,
            },
            &ctx,
        );
        // The waiter fails FAST rather than being answered with a fabricated
        // empty snapshot: its sender is dropped, so the requester sees a closed
        // channel instead of "the venue says you have no orders".
        assert!(
            matches!(
                orders_rx.try_recv(),
                Err(oneshot::error::TryRecvError::Closed)
            ),
            "the refused order query's waiter is dropped"
        );
        assert!(
            matches!(
                fills_rx.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ),
            "the same-id fill waiter is untouched: the wrong map is never probed"
        );
    }

    /// A fake venue behind the exec command channel: answers the venue-truth
    /// queries from canned rows (applying the same id/open_only filtering the
    /// engine does), resolving waiters through the same pending map the real
    /// WS reader uses. Every other command is swallowed.
    fn install_fake_venue(
        client: &mut MogwaiExecutionClient,
        orders: Vec<OrderStatusInfo>,
        fills: Vec<mogwai_protocol::OrderFilled>,
    ) -> tokio::task::JoinHandle<()> {
        let (tx, mut rx) = unbounded_channel::<ExecWsCommand>();
        client.ws_cmd = Some(tx);
        let pending = Arc::clone(&client.pending);
        tokio::spawn(async move {
            while let Some(cmd) = rx.recv().await {
                match cmd {
                    ExecWsCommand::QueryOrders {
                        request_id,
                        client_order_id,
                        open_only,
                    } => {
                        let rows = orders
                            .iter()
                            .filter(|info| match &client_order_id {
                                Some(id) => info.client_order_id == *id,
                                None => !open_only || info.status.is_open(),
                            })
                            .cloned()
                            .collect();
                        let waiter = pending
                            .lock()
                            .expect("pending queries mutex")
                            .orders
                            .remove(&request_id);
                        if let Some(reply) = waiter {
                            drop(reply.send(OrderStatusSnapshot {
                                request_id,
                                orders: rows,
                                ts_event: 99,
                            }));
                        }
                    }
                    ExecWsCommand::QueryFills {
                        request_id,
                        client_order_id,
                    } => {
                        let rows = fills
                            .iter()
                            .filter(|fill| {
                                client_order_id
                                    .as_ref()
                                    .is_none_or(|id| fill.client_order_id == *id)
                            })
                            .cloned()
                            .collect();
                        let waiter = pending
                            .lock()
                            .expect("pending queries mutex")
                            .fills
                            .remove(&request_id);
                        if let Some(reply) = waiter {
                            drop(reply.send(FillSnapshot {
                                request_id,
                                fills: rows,
                                ts_event: 99,
                            }));
                        }
                    }
                    _ => {}
                }
            }
        })
    }

    fn wire_order_info(
        id: &str,
        status: WireOrderStatus,
        filled: Decimal,
        ts: u64,
    ) -> OrderStatusInfo {
        OrderStatusInfo {
            client_order_id: id.into(),
            venue_order_id: "V-1".into(),
            symbol: "BTCUSDT".into(),
            side: Side::Buy,
            order_type: mogwai_protocol::OrderType::Limit,
            time_in_force: mogwai_protocol::TimeInForce::Gtc,
            status,
            quantity: Decimal::new(1, 0),
            filled_qty: filled,
            price: Some(Decimal::new(10_000, 2)),
            ts_accepted: ts,
            ts_last: ts,
        }
    }

    #[test]
    fn mogwai_exec_client_start_stop_are_idempotent() {
        let mut client = execution_client();

        assert!(client.is_stopped());
        assert!(!client.is_started());

        client.start().expect("first start succeeds");
        client.start().expect("second start succeeds");
        assert!(client.is_started());
        assert!(!client.is_stopped());

        client.stop().expect("first stop succeeds");
        client.stop().expect("second stop succeeds");
        assert!(client.is_stopped());
        assert!(!client.is_started());
        assert!(!client.is_connected());
    }

    #[test]
    fn reset_clears_exec_state_so_orders_do_not_leak_across_sessions() {
        let mut client = execution_client();
        seed_order(&client.state);
        client
            .state
            .lock()
            .expect("execution state mutex")
            .account_ts_last = UnixNanos::from(42);

        client.reset().expect("reset succeeds");

        let state = client.state.lock().expect("execution state mutex");
        assert!(state.orders.is_empty(), "orders cleared on reset");
        assert_eq!(
            state.account_ts_last,
            UnixNanos::default(),
            "the account staleness watermark resets with the session; carrying it \
             over would make a new session's first snapshot look stale and drop it"
        );
    }

    #[test]
    fn submit_modify_cancel_emit_wire_commands() {
        let submit = ExecWsCommand::Submit(mogwai_protocol::SubmitOrder {
            client_order_id: "O-1".into(),
            symbol: "BTCUSDT".into(),
            side: Side::Buy,
            order_type: mogwai_protocol::OrderType::Limit,
            quantity: Decimal::new(1, 0),
            price: Some(Decimal::new(10_000, 2)),
            time_in_force: mogwai_protocol::TimeInForce::Gtc,
        });

        assert!(matches!(
            exec_command_to_client_message(submit),
            ClientMessage::SubmitOrder(order)
                if order.client_order_id == "O-1"
                    && order.symbol == "BTCUSDT"
                    && order.side == Side::Buy
        ));
        assert!(matches!(
            exec_command_to_client_message(ExecWsCommand::Modify {
                client_order_id: "O-1".into(),
                price: Some(Decimal::new(12_000, 2)),
                quantity: Some(Decimal::new(2, 0)),
            }),
            ClientMessage::ModifyOrder {
                client_order_id,
                price: Some(price),
                quantity: Some(quantity),
            } if client_order_id == "O-1"
                && price == Decimal::new(12_000, 2)
                && quantity == Decimal::new(2, 0)
        ));
        assert!(matches!(
            exec_command_to_client_message(ExecWsCommand::Cancel {
                client_order_id: "O-1".into(),
            }),
            ClientMessage::CancelOrder { client_order_id } if client_order_id == "O-1"
        ));
    }

    #[tokio::test]
    async fn http_orders_dispatch_does_not_require_ws_channel() {
        let client = execution_client_with_config(MogwaiExecClientConfig {
            transport_profile: TransportProfile::HttpOrders,
            ..MogwaiExecClientConfig::default()
        });

        client
            .dispatch_order(ExecWsCommand::Cancel {
                client_order_id: "O-1".into(),
            })
            .expect("HTTP dispatch accepts command without websocket");
        assert!(client.ws_cmd.is_none());
    }

    #[test]
    fn account_state_buckets_into_exec_latency_not_data() {
        // Distinct knobs so a misbucket is observable: base 5ns, exec +20,
        // fill +30, data +40. The always-on 30ms baseline adds on top of those
        // armed values. The bug this pins had `AccountState` riding the data
        // knob; it is an account/execution event and must take exec latency.
        // Both ends key off `ServerMessage::category`, so this asserts the
        // adapter side of that shared classification.
        let havoc = ClientHavoc {
            latency: Some(HavocLatency {
                base_nanos: 5,
                exec_event_nanos: 20,
                fill_nanos: 30,
                data_nanos: 40,
            }),
            ..ClientHavoc::default()
        };
        let filter = HavocFilter::from_client(&havoc);

        let account = ServerMessage::AccountState(mogwai_protocol::AccountState {
            account_id: mogwai_protocol::AccountId::parse("MOGWAI-001").unwrap(),
            balances: Vec::new(),
            positions: Vec::new(),
            ts_event: 1,
        });
        assert_eq!(
            filter.delay_for(&account),
            Duration::from_nanos(30_000_025),
            "AccountState takes baseline + armed base + exec latency, not data"
        );

        let trade = ServerMessage::Trade(TradeTick {
            symbol: "BTCUSDT".into(),
            price: Decimal::new(1, 0),
            size: Decimal::new(1, 0),
            aggressor: mogwai_protocol::AggressorSide::NoAggressor,
            ts_event: 1,
        });
        assert_eq!(
            filter.delay_for(&trade),
            Duration::from_nanos(30_000_045),
            "trades still take baseline + armed base + data latency"
        );

        let fill = ServerMessage::OrderFilled(mogwai_protocol::OrderFilled {
            client_order_id: "O-1".into(),
            venue_order_id: "V-1".into(),
            trade_id: "T-1".into(),
            symbol: "BTCUSDT".into(),
            side: Side::Buy,
            last_qty: Decimal::new(1, 0),
            last_px: Decimal::new(10_000, 2),
            leaves_qty: Decimal::ZERO,
            commission: Decimal::ZERO,
            ts_event: 1,
        });
        assert_eq!(
            filter.delay_for(&fill),
            Duration::from_nanos(30_000_035),
            "fills still take baseline + armed base + fill latency"
        );
    }

    #[test]
    fn no_armed_latency_still_carries_baseline() {
        // `ClientHavoc` with no latency covers both no-havoc and an explicit
        // client object without a latency key. It must still carry the honest
        // baseline, and armed latency must add on top rather than replace it.
        let filter = HavocFilter::from_client(&ClientHavoc::default());
        let trade = ServerMessage::Trade(TradeTick {
            symbol: "BTCUSDT".into(),
            price: Decimal::new(1, 0),
            size: Decimal::new(1, 0),
            aggressor: mogwai_protocol::AggressorSide::NoAggressor,
            ts_event: 1,
        });
        assert_eq!(
            filter.delay_for(&trade),
            mogwai_protocol::BASELINE_LATENCY.delay_for(mogwai_protocol::EventKind::Data)
        );

        let armed = HavocFilter::from_client(&ClientHavoc {
            latency: Some(HavocLatency {
                base_nanos: 7,
                ..Default::default()
            }),
            ..ClientHavoc::default()
        });
        assert_eq!(
            armed.delay_for(&trade),
            mogwai_protocol::BASELINE_LATENCY.delay_for(mogwai_protocol::EventKind::Data)
                + Duration::from_nanos(7)
        );
    }

    #[test]
    fn draw_at_intermediate_probability_is_seeded_and_mixed() {
        // F.6: every other havoc test pins the probability to 1.0 or 0.0, so the
        // `> 0.0` short-circuit (false) or the always-true `r#gen::<f64>() < 1.0`
        // arm (true) decides every draw and the seeded RNG is never consulted.
        // A regression in the seeding (wrong seed plumbed in, `from_entropy`
        // taken instead) or the draw arithmetic (`<=` vs `<`) would slip through
        // silently. This pins a mid-range probability against a fixed seed and
        // asserts the exact fire/no-fire sequence over several draws, so it is
        // sensitive to both the seed and the comparison and is robust to nothing
        // but a real behavior change.
        //
        // The expected sequence below is the literal output of `StdRng`
        // seed_from_u64(SEED) drawing `gen::<f64> < 0.5` ten times. It is a
        // genuine mix of true and false (asserted), so a draw that ignored the
        // RNG and returned a constant could not reproduce it. The same SEED on a
        // second filter must reproduce the run bit for bit (determinism); a
        // different seed must diverge somewhere (the seed is actually consulted).
        const SEED: u64 = 0xC0FF_EE12_3456_789A;
        const PROBABILITY: f64 = 0.5;

        let havoc = ClientHavoc {
            seed: Some(SEED),
            ..ClientHavoc::default()
        };

        let mut filter = HavocFilter::from_client(&havoc);
        let actual: Vec<bool> = (0..10).map(|_| filter.draw(PROBABILITY)).collect();

        let expected = [
            true, true, true, false, true, true, false, false, true, true,
        ];
        assert_eq!(
            actual, expected,
            "seeded draw sequence drifted: seeding or draw arithmetic changed"
        );

        // Guard against the sequence degenerating to all-one-way, which would
        // make the test pass even if the draw ignored its argument or its RNG.
        assert!(
            expected.iter().any(|&b| b) && expected.iter().any(|&b| !b),
            "the pinned sequence must mix fire and no-fire to exercise the draw"
        );

        // Same seed reproduces the run exactly (determinism), so the outcome is a
        // function of the seed and not of process entropy.
        let mut twin = HavocFilter::from_client(&havoc);
        let twin_seq: Vec<bool> = (0..10).map(|_| twin.draw(PROBABILITY)).collect();
        assert_eq!(twin_seq, expected, "identical seed must replay identically");

        // A different seed must change the sequence somewhere; if it did not, the
        // seed would not be reaching the RNG at all.
        let other = ClientHavoc {
            seed: Some(SEED ^ 1),
            ..ClientHavoc::default()
        };
        let mut other_filter = HavocFilter::from_client(&other);
        let other_seq: Vec<bool> = (0..10).map(|_| other_filter.draw(PROBABILITY)).collect();
        assert_ne!(
            other_seq, expected,
            "a different seed must produce a different draw sequence"
        );
    }

    #[test]
    fn per_dispatch_seed_decorrelates_yet_stays_reproducible() {
        // F.6 follow-up: `client_havoc_for_dispatch` XORs the configured client
        // seed with the per-dispatch counter (`*seed ^= counter`) so each
        // dispatched order draws a distinct havoc stream instead of every order
        // replaying one. A regression dropping the XOR would silently collapse
        // all dispatches onto the same sequence; that bug is invisible to the
        // single-stream `draw` test above. This pins a configured seed plus a
        // mid-range probability, builds the filter the way `dispatch_order`
        // does (derive a per-dispatch ClientHavoc, then HavocFilter::from_client),
        // and asserts adjacent counters DECORRELATE while each counter stays
        // reproducible under the same configured seed.
        const SEED: u64 = 0xC0FF_EE12_3456_789A;
        const PROBABILITY: f64 = 0.5;

        let spec = Some(HavocSpec {
            client: ClientHavoc {
                seed: Some(SEED),
                drop_prob: PROBABILITY,
                ..ClientHavoc::default()
            },
            ..HavocSpec::default()
        });

        // The seam under test: a per-dispatch ClientHavoc whose seed is the
        // configured seed XORed with the dispatch counter.
        let draw_dispatch = |counter: u64| -> Vec<bool> {
            let client = client_havoc_for_dispatch(&spec, counter);
            let mut filter = HavocFilter::from_client(&client);
            (0..10).map(|_| filter.draw(PROBABILITY)).collect()
        };

        // Confirm the XOR actually moved the seed off the configured value for a
        // non-zero counter (counter 0 leaves it untouched, which is intended).
        assert_eq!(
            client_havoc_for_dispatch(&spec, 0).seed,
            Some(SEED),
            "counter 0 must leave the configured seed untouched"
        );
        assert_eq!(
            client_havoc_for_dispatch(&spec, 1).seed,
            Some(SEED ^ 1),
            "counter 1 must XOR the configured seed with the counter"
        );

        let first = draw_dispatch(0);
        let second = draw_dispatch(1);

        // Decorrelation: two successive dispatches must draw different sequences.
        // If the XOR were dropped, both would replay the configured seed and
        // these would be identical.
        assert_ne!(
            first, second,
            "successive dispatches must draw decorrelated havoc streams; \
             the per-dispatch seed XOR was dropped if these match"
        );

        // Each must mix fire and no-fire so the comparison exercises the draw
        // rather than passing on a degenerate all-one-way run.
        assert!(
            first.iter().any(|&b| b) && first.iter().any(|&b| !b),
            "dispatch 0 must mix fire and no-fire"
        );
        assert!(
            second.iter().any(|&b| b) && second.iter().any(|&b| !b),
            "dispatch 1 must mix fire and no-fire"
        );

        // Individual reproducibility: the SAME configured seed must replay each
        // per-dispatch stream bit for bit, so the decorrelation is deterministic
        // and not process entropy. Re-deriving from the same `spec` reproduces.
        assert_eq!(
            draw_dispatch(0),
            first,
            "same configured seed must replay dispatch 0 identically"
        );
        assert_eq!(
            draw_dispatch(1),
            second,
            "same configured seed must replay dispatch 1 identically"
        );
    }

    #[test]
    fn accepted_then_filled_then_account_drive_exec_events() {
        let (ctx, mut rx) = exec_context();

        handle_exec_message(
            ServerMessage::OrderAccepted {
                client_order_id: "O-1".into(),
                venue_order_id: "V-1".into(),
                ts_event: 10,
            },
            &ctx,
        );
        handle_exec_message(
            ServerMessage::OrderFilled(mogwai_protocol::OrderFilled {
                client_order_id: "O-1".into(),
                venue_order_id: "V-1".into(),
                trade_id: "T-1".into(),
                symbol: "BTCUSDT".into(),
                side: Side::Buy,
                last_qty: Decimal::new(1, 0),
                last_px: Decimal::new(10_000, 2),
                leaves_qty: Decimal::ZERO,
                commission: Decimal::ZERO,
                ts_event: 11,
            }),
            &ctx,
        );
        handle_exec_message(
            ServerMessage::AccountState(mogwai_protocol::AccountState {
                account_id: mogwai_protocol::AccountId::parse("MOGWAI-001").unwrap(),
                balances: vec![mogwai_protocol::Balance {
                    currency: "USDT".into(),
                    total: Decimal::new(10_000, 0),
                    free: Decimal::new(9_900, 0),
                    locked: Decimal::new(100, 0),
                }],
                positions: vec![mogwai_protocol::Position {
                    symbol: "BTCUSDT".into(),
                    quantity: Decimal::new(1, 0),
                    avg_px: Decimal::new(10_000, 2),
                }],
                ts_event: 12,
            }),
            &ctx,
        );

        assert!(matches!(
            rx.try_recv().expect("accepted event"),
            ExecutionEvent::Order(OrderEventAny::Accepted(_))
        ));
        assert!(matches!(
            rx.try_recv().expect("filled event"),
            ExecutionEvent::Order(OrderEventAny::Filled(_))
        ));
        assert!(matches!(
            rx.try_recv().expect("account event"),
            ExecutionEvent::Account(_)
        ));
    }

    #[test]
    fn http_response_events_use_same_exec_drain() {
        let (ctx, mut rx) = exec_context();
        let events = vec![
            ServerMessage::OrderAccepted {
                client_order_id: "O-1".into(),
                venue_order_id: "V-1".into(),
                ts_event: 10,
            },
            ServerMessage::OrderFilled(mogwai_protocol::OrderFilled {
                client_order_id: "O-1".into(),
                venue_order_id: "V-1".into(),
                trade_id: "T-1".into(),
                symbol: "BTCUSDT".into(),
                side: Side::Buy,
                last_qty: Decimal::new(1, 0),
                last_px: Decimal::new(10_000, 2),
                leaves_qty: Decimal::ZERO,
                commission: Decimal::ZERO,
                ts_event: 11,
            }),
        ];

        for event in events {
            handle_exec_message(event, &ctx);
        }

        assert!(matches!(
            rx.try_recv().expect("accepted event"),
            ExecutionEvent::Order(OrderEventAny::Accepted(_))
        ));
        assert!(matches!(
            rx.try_recv().expect("filled event"),
            ExecutionEvent::Order(OrderEventAny::Filled(_))
        ));
    }

    #[test]
    fn http_post_failure_rejects_submitted_order() {
        let (ctx, mut rx) = exec_context();
        let cmd = ExecWsCommand::Submit(mogwai_protocol::SubmitOrder {
            client_order_id: "O-1".into(),
            symbol: "BTCUSDT".into(),
            side: Side::Buy,
            order_type: mogwai_protocol::OrderType::Limit,
            quantity: Decimal::new(1, 0),
            price: Some(Decimal::new(10_000, 2)),
            time_in_force: mogwai_protocol::TimeInForce::Gtc,
        });
        let err = anyhow::anyhow!("connection refused");

        handle_exec_message(reject_for(&cmd, &err, SimClock::identity()), &ctx);

        match rx.try_recv().expect("rejected event") {
            ExecutionEvent::Order(OrderEventAny::Rejected(event)) => {
                assert_eq!(event.client_order_id, ClientOrderId::from("O-1"));
                assert_eq!(event.reason.as_str(), "connection refused");
            }
            other => panic!("expected rejected event, got {other:?}"),
        }
        let record = order_record(&ctx.state, ClientOrderId::from("O-1")).expect("order record");
        assert_eq!(record.status, OrderStatus::Rejected);
    }

    #[test]
    fn http_cancel_failure_reports_cancel_rejected_without_touching_status() {
        // A cancel failing at transport must not be reported as a full order
        // rejection (bug-hunt A.3): the order never heard back from the
        // venue, so it is still whatever it was before the cancel attempt.
        let (ctx, mut rx) = exec_context();
        let err = anyhow::anyhow!("connection refused");

        emit_cancel_rejected(
            ClientOrderId::from("O-1"),
            None,
            err.to_string(),
            now_unix_nanos(ctx.sim),
            &ctx,
        );

        match rx.try_recv().expect("cancel rejected event") {
            ExecutionEvent::Order(OrderEventAny::CancelRejected(event)) => {
                assert_eq!(event.client_order_id, ClientOrderId::from("O-1"));
                assert_eq!(event.reason.as_str(), "connection refused");
            }
            other => panic!("expected cancel rejected event, got {other:?}"),
        }
        let record = order_record(&ctx.state, ClientOrderId::from("O-1")).expect("order record");
        assert_eq!(
            record.status,
            OrderStatus::Submitted,
            "a failed cancel must leave the mirrored order status untouched"
        );
    }

    #[test]
    fn wire_cancel_rejected_surfaces_event_and_honors_wire_venue_id() {
        // A venue-originated ServerMessage::OrderCancelRejected (the engine
        // could not honor a cancel: target unknown or already terminal) must
        // surface as a nautilus CancelRejected, leave the mirrored order's
        // status untouched (like the transport-failure path), and carry the
        // venue id named on the wire even though the seeded record holds None.
        let (ctx, mut rx) = exec_context();

        handle_exec_message(
            ServerMessage::OrderCancelRejected {
                client_order_id: "O-1".into(),
                venue_order_id: Some("V-1".into()),
                reason: "order already terminal (filled or canceled)".into(),
                ts_event: 42,
            },
            &ctx,
        );

        match rx.try_recv().expect("cancel rejected event") {
            ExecutionEvent::Order(OrderEventAny::CancelRejected(event)) => {
                assert_eq!(event.client_order_id, ClientOrderId::from("O-1"));
                assert_eq!(
                    event.reason.as_str(),
                    "order already terminal (filled or canceled)"
                );
                assert_eq!(event.venue_order_id, Some(VenueOrderId::from("V-1")));
                assert_eq!(event.ts_event, UnixNanos::from(42));
            }
            other => panic!("expected cancel rejected event, got {other:?}"),
        }
        let record = order_record(&ctx.state, ClientOrderId::from("O-1")).expect("order record");
        assert_eq!(
            record.status,
            OrderStatus::Submitted,
            "a rejected cancel must leave the mirrored order status untouched"
        );
    }

    #[test]
    fn emitter_bypass_events_stamp_ts_init_on_sim_axis() {
        let (mut ctx, mut rx) = exec_context();
        let wall = mogwai_protocol::now_unix_nanos();
        ctx.sim = SimClock {
            sim_epoch_ns: 1_900_000_000_000_000_000,
            wall_anchor_ns: wall.saturating_sub(1),
            speed: 1.0,
        };

        handle_exec_message(
            ServerMessage::OrderRejected {
                client_order_id: "O-1".into(),
                reason: "no".into(),
                ts_event: 10,
            },
            &ctx,
        );
        match rx.try_recv().expect("rejected event") {
            ExecutionEvent::Order(OrderEventAny::Rejected(event)) => {
                assert!(event.ts_init.as_u64() >= ctx.sim.sim_epoch_ns);
            }
            other => panic!("expected rejected event, got {other:?}"),
        }

        handle_exec_message(
            ServerMessage::AccountState(mogwai_protocol::AccountState {
                account_id: mogwai_protocol::AccountId::parse("MOGWAI-001").unwrap(),
                balances: Vec::new(),
                positions: Vec::new(),
                ts_event: 12,
            }),
            &ctx,
        );
        match rx.try_recv().expect("account event") {
            ExecutionEvent::Account(event) => {
                assert!(event.ts_init.as_u64() >= ctx.sim.sim_epoch_ns);
            }
            other => panic!("expected account event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn reports_repeat_venue_truth_not_the_mirror() {
        // The poll-heal fault class end to end at the adapter seam: the
        // mirror believes O-1 still rests open (its cancel event was
        // dropped), while the venue's truth says Canceled. The report
        // generators must repeat the venue - a mirror-based report would
        // confidently confirm the stale open order, which is exactly the
        // corruption reconciliation exists to catch (AE2).
        let mut client = execution_client();
        client.instruments = instruments_map();
        seed_order(&client.state);
        client
            .state
            .lock()
            .expect("state")
            .orders
            .get_mut(&ClientOrderId::from("O-1"))
            .expect("seeded order")
            .status = OrderStatus::Accepted;
        let _venue = install_fake_venue(
            &mut client,
            vec![wire_order_info(
                "O-1",
                WireOrderStatus::Canceled,
                Decimal::ZERO,
                11,
            )],
            vec![wire_fill("T-1", Decimal::ZERO, 11)],
        );
        let _account =
            install_account_venue(&mut client, vec![wire_position(Decimal::new(1, 0))], 12).await;

        let orders = client
            .generate_order_status_reports(&GenerateOrderStatusReports::new(
                UUID4::new(),
                UnixNanos::from(20),
                false,
                Some(instrument_id()),
                None,
                None,
                None,
                None,
            ))
            .await
            .expect("order reports");
        let fills = client
            .generate_fill_reports(GenerateFillReports::new(
                UUID4::new(),
                UnixNanos::from(20),
                Some(instrument_id()),
                Some(VenueOrderId::from("V-1")),
                None,
                None,
                None,
                None,
            ))
            .await
            .expect("fill reports");
        let positions = client
            .generate_position_status_reports(&GeneratePositionStatusReports::new(
                UUID4::new(),
                UnixNanos::from(20),
                Some(instrument_id()),
                None,
                None,
                None,
                None,
            ))
            .await
            .expect("position reports");

        assert_eq!(orders.len(), 1);
        assert_eq!(
            orders[0].order_status,
            OrderStatus::Canceled,
            "the report must repeat the venue's Canceled, not the mirror's stale Accepted"
        );
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].trade_id, TradeId::from("T-1"));
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].position_side, PositionSideSpecified::Long);
    }

    #[tokio::test]
    async fn singular_report_and_unanswered_query_paths() {
        // The singular generator (backing the in-flight re-query) resolves a
        // targeted order from venue truth; a venue that knows nothing of the
        // id yields Ok(None), and a venue that never replies at all surfaces
        // as a timeout error - the delivery-havoc path (DelayAcks/GoDark on
        // the reply) that exercises the consumer's own query timeout.
        let mut client = execution_client();
        client.instruments = instruments_map();
        let _venue = install_fake_venue(
            &mut client,
            vec![wire_order_info(
                "O-1",
                WireOrderStatus::PartiallyFilled,
                Decimal::new(1, 0),
                11,
            )],
            Vec::new(),
        );

        let report = client
            .generate_order_status_report(&GenerateOrderStatusReport::new(
                UUID4::new(),
                UnixNanos::from(20),
                None,
                Some(ClientOrderId::from("O-1")),
                None,
                None,
                None,
            ))
            .await
            .expect("targeted report generates");
        let report = report.expect("the venue knows O-1");
        assert_eq!(report.order_status, OrderStatus::PartiallyFilled);
        assert_eq!(report.venue_order_id, VenueOrderId::from("V-1"));

        let ghost = client
            .generate_order_status_report(&GenerateOrderStatusReport::new(
                UUID4::new(),
                UnixNanos::from(20),
                None,
                Some(ClientOrderId::from("GHOST")),
                None,
                None,
                None,
            ))
            .await
            .expect("unknown id still generates");
        assert!(
            ghost.is_none(),
            "an id the venue never accepted reports as absent, not as an error"
        );
    }

    #[tokio::test]
    async fn unanswered_ws_query_times_out_instead_of_hanging() {
        // A venue that swallows the reply (GoDark on the exec socket): the
        // query must fail with a timeout after the request-timeout window,
        // and the pending slot must be cleaned up rather than leaked. Built
        // directly with a 1s timeout so the test does not wait out the
        // production 30s default.
        let client = execution_client();
        let (tx, _rx) = unbounded_channel::<ExecWsCommand>();
        let query = VenueQuery {
            http: None,
            ws_cmd: Some(tx),
            pending: Arc::clone(&client.pending),
            timeout_secs: 1,
        };

        let err = query
            .order_status(None, true)
            .await
            .expect_err("no reply must time out");
        assert!(
            err.to_string().contains("timed out"),
            "the error names the timeout: {err}"
        );
        assert!(
            client
                .pending
                .lock()
                .expect("pending queries mutex")
                .orders
                .is_empty(),
            "a timed-out query must not leak its pending slot"
        );
    }

    #[tokio::test]
    async fn position_report_follows_the_venue_when_the_closing_snapshot_is_dropped() {
        // The DropNextAccountUpdate fault class, which is the whole reason
        // these reports moved off the client-side mirror. The client sees the
        // ENTRY snapshot (long 1) and then never sees the CLOSE - that push is
        // the one the divergence swallows - so any client-side belief about
        // this position is stale-long forever. The venue is flat, and the
        // report must say flat: a stale-long report is adopted downstream as a
        // phantom EXTERNAL position, which desyncs attribution (slices no
        // longer sum to net) and halts the account.
        let mut client = execution_client();
        client.instruments = instruments_map();
        seed_order(&client.state);
        let (ctx, _rx) = exec_context();
        let ctx = ExecContext {
            emitter: ctx.emitter,
            state: Arc::clone(&client.state),
            instruments: Arc::clone(&client.instruments),
            pending: Arc::clone(&client.pending),
            trader_id: ctx.trader_id,
            account_id: ctx.account_id,
            account_type: ctx.account_type,
            sim: ctx.sim,
        };
        handle_exec_message(
            ServerMessage::AccountState(mogwai_protocol::AccountState {
                account_id: mogwai_protocol::AccountId::parse("MOGWAI-001").unwrap(),
                balances: Vec::new(),
                positions: vec![wire_position(Decimal::new(1, 0))],
                ts_event: 12,
            }),
            &ctx,
        );

        // The venue's own truth: flat. The engine removes a position closed to
        // zero rather than reporting a zero-qty row, so "flat" is an absent
        // row, not a zero one.
        let _account = install_account_venue(&mut client, Vec::new(), 13).await;

        let positions = client
            .generate_position_status_reports(&GeneratePositionStatusReports::new(
                UUID4::new(),
                UnixNanos::from(20),
                Some(instrument_id()),
                None,
                None,
                None,
                None,
            ))
            .await
            .expect("position reports");

        assert!(
            positions.is_empty(),
            "the report must follow the venue's flat truth, not the last snapshot the client happened to see"
        );
    }

    fn wire_fill(
        trade_id: &str,
        leaves_qty: Decimal,
        ts_event: u64,
    ) -> mogwai_protocol::OrderFilled {
        mogwai_protocol::OrderFilled {
            client_order_id: "O-1".into(),
            venue_order_id: "V-1".into(),
            trade_id: trade_id.into(),
            symbol: "BTCUSDT".into(),
            side: Side::Buy,
            last_qty: Decimal::new(1, 0),
            last_px: Decimal::new(10_000, 2),
            leaves_qty,
            commission: Decimal::ZERO,
            ts_event,
        }
    }

    #[test]
    fn reordered_terminal_events_cannot_regress_the_mirror() {
        // Reorder havoc transposes the engine's adjacent Accepted+Filled pair
        // (immediate fills), so the mirror sees Filled first and the Accepted
        // arrives late. The old unconditional overwrite regressed the mirror
        // to Accepted FOREVER (nothing later corrects a terminal order), and
        // generate_order_status_reports(open_only) then reported a phantom
        // open order with full filled_qty. The mirror must be at least as
        // strict as nautilus's own FSM, which has no terminal-to-Accepted arm.
        let (ctx, mut rx) = exec_context();

        handle_exec_message(
            ServerMessage::OrderAccepted {
                client_order_id: "O-1".into(),
                venue_order_id: "V-1".into(),
                ts_event: 10,
            },
            &ctx,
        );
        handle_exec_message(
            ServerMessage::OrderFilled(wire_fill("T-1", Decimal::ZERO, 11)),
            &ctx,
        );
        let _ = rx.try_recv().expect("accepted");
        let _ = rx.try_recv().expect("filled");

        // The reordered duplicate Accepted lands after the fill: the wire
        // event is still forwarded (nautilus's FSM refuses it on its own),
        // but the mirror keeps its terminal status and timestamps.
        handle_exec_message(
            ServerMessage::OrderAccepted {
                client_order_id: "O-1".into(),
                venue_order_id: "V-1".into(),
                ts_event: 10,
            },
            &ctx,
        );
        assert!(matches!(
            rx.try_recv().expect("late accepted still forwarded"),
            ExecutionEvent::Order(OrderEventAny::Accepted(_))
        ));
        let record = order_record(&ctx.state, ClientOrderId::from("O-1")).expect("order record");
        assert_eq!(
            record.status,
            OrderStatus::Filled,
            "a late Accepted must not regress a Filled mirror record"
        );
        assert_eq!(record.ts_last, UnixNanos::from(11));

        // A Canceled transposed behind the fill likewise must not overwrite.
        handle_exec_message(
            ServerMessage::OrderCanceled {
                client_order_id: "O-1".into(),
                venue_order_id: "V-1".into(),
                ts_event: 12,
            },
            &ctx,
        );
        let record = order_record(&ctx.state, ClientOrderId::from("O-1")).expect("order record");
        assert_eq!(
            record.status,
            OrderStatus::Filled,
            "a late Canceled must not overwrite a Filled mirror record"
        );

        // And an amend ack reordered behind the terminal event must not
        // recompute a non-terminal status from leaves_qty.
        handle_exec_message(
            ServerMessage::OrderUpdated {
                client_order_id: "O-1".into(),
                venue_order_id: "V-1".into(),
                quantity: Decimal::new(2, 0),
                price: Some(Decimal::new(10_000, 2)),
                leaves_qty: Decimal::new(2, 0),
                ts_event: 9,
            },
            &ctx,
        );
        let record = order_record(&ctx.state, ClientOrderId::from("O-1")).expect("order record");
        assert_eq!(
            record.status,
            OrderStatus::Filled,
            "a late Updated must not regress a Filled mirror record"
        );
        assert_eq!(
            record.quantity,
            Decimal::new(1, 0),
            "a late amend must not rewrite a terminal record's quantity"
        );
    }

    #[test]
    fn late_fill_after_cancel_books_economics_without_reopening() {
        // The engine's partial-fill-then-cancel pair (e.g. IOC remainder)
        // transposed by reorder havoc: Canceled first, then the partial fill.
        // Money moved at the venue, so the fill's economics must book into
        // the mirror - but the terminal Canceled status must survive, and
        // ts_last must not walk backward to the fill's older stamp.
        let (ctx, mut rx) = exec_context();

        handle_exec_message(
            ServerMessage::OrderAccepted {
                client_order_id: "O-1".into(),
                venue_order_id: "V-1".into(),
                ts_event: 10,
            },
            &ctx,
        );
        handle_exec_message(
            ServerMessage::OrderCanceled {
                client_order_id: "O-1".into(),
                venue_order_id: "V-1".into(),
                ts_event: 12,
            },
            &ctx,
        );
        let _ = rx.try_recv().expect("accepted");
        let _ = rx.try_recv().expect("canceled");

        // The partial fill (leaves > 0) arrives late.
        handle_exec_message(
            ServerMessage::OrderFilled(wire_fill("T-1", Decimal::new(1, 0), 11)),
            &ctx,
        );
        assert!(matches!(
            rx.try_recv().expect("late fill still forwarded"),
            ExecutionEvent::Order(OrderEventAny::Filled(_))
        ));
        let record = order_record(&ctx.state, ClientOrderId::from("O-1")).expect("order record");
        assert_eq!(
            record.status,
            OrderStatus::Canceled,
            "a late partial fill must not re-open a Canceled mirror record"
        );
        assert_eq!(
            record.filled_qty,
            Decimal::new(1, 0),
            "the late fill's economics must still book"
        );
        assert_eq!(
            record.ts_last,
            UnixNanos::from(12),
            "ts_last must not walk backward to the reordered fill's stamp"
        );
    }

    #[test]
    fn stale_account_snapshot_is_dropped_entirely() {
        // Two adjacent AccountStates transposed by reorder havoc: the close
        // snapshot (newer, position gone) arrives first, then the entry
        // snapshot (older, position open). Applying the older one in arrival
        // order resurrected the closed position - the phantom-EXTERNAL class -
        // and moved the mirror's watermark backward. The stale snapshot must
        // be skipped wholesale: no mirror mutation, no account event forwarded
        // (nautilus has no staleness guard of its own).
        let (ctx, mut rx) = exec_context();

        handle_exec_message(
            ServerMessage::AccountState(mogwai_protocol::AccountState {
                account_id: mogwai_protocol::AccountId::parse("MOGWAI-001").unwrap(),
                balances: Vec::new(),
                positions: Vec::new(),
                ts_event: 13,
            }),
            &ctx,
        );
        assert!(matches!(
            rx.try_recv().expect("fresh snapshot forwards"),
            ExecutionEvent::Account(_)
        ));

        handle_exec_message(
            ServerMessage::AccountState(mogwai_protocol::AccountState {
                account_id: mogwai_protocol::AccountId::parse("MOGWAI-001").unwrap(),
                balances: Vec::new(),
                positions: vec![mogwai_protocol::Position {
                    symbol: "BTCUSDT".into(),
                    quantity: Decimal::new(1, 0),
                    avg_px: Decimal::new(10_000, 2),
                }],
                ts_event: 12,
            }),
            &ctx,
        );
        assert!(
            rx.try_recv().is_err(),
            "a stale snapshot must not forward an account event"
        );
        let state = ctx.state.lock().expect("execution state mutex");
        assert_eq!(
            state.account_ts_last,
            UnixNanos::from(13),
            "the applied watermark must not move backward"
        );
    }

    #[test]
    fn modify_reject_falls_back_to_mirror_venue_id() {
        // The engine omits the venue id on a reject for an order that has
        // gone terminal even though the id is known. emit_cancel_rejected
        // already falls back to the mirror's id; the modify path must match,
        // so a known order's modify-reject carries the id the adapter holds.
        let (ctx, mut rx) = exec_context();

        handle_exec_message(
            ServerMessage::OrderAccepted {
                client_order_id: "O-1".into(),
                venue_order_id: "V-1".into(),
                ts_event: 10,
            },
            &ctx,
        );
        let _ = rx.try_recv().expect("accepted");

        handle_exec_message(
            ServerMessage::OrderModifyRejected {
                client_order_id: "O-1".into(),
                venue_order_id: None,
                reason: "order already terminal (filled or canceled)".into(),
                ts_event: 42,
            },
            &ctx,
        );
        match rx.try_recv().expect("modify rejected event") {
            ExecutionEvent::Order(OrderEventAny::ModifyRejected(event)) => {
                assert_eq!(
                    event.venue_order_id,
                    Some(VenueOrderId::from("V-1")),
                    "a wire None must fall back to the mirror's venue id"
                );
            }
            other => panic!("expected modify rejected event, got {other:?}"),
        }
    }

    #[test]
    fn hostile_fill_trade_id_drops_fill_without_panicking() {
        // Nautilus caps TradeId at 36 non-empty ASCII chars and the panicking
        // From impl asserts past that. A server-sent or havoc-corrupted id
        // must drop the fill with a warning instead of panicking the
        // unsupervised exec task; the mirror stays untouched.
        let (ctx, mut rx) = exec_context();
        let long_id = "T-".repeat(30);

        handle_exec_message(
            ServerMessage::OrderFilled(wire_fill(&long_id, Decimal::ZERO, 11)),
            &ctx,
        );

        assert!(
            rx.try_recv().is_err(),
            "an unrepresentable trade id must not emit a fill event"
        );
        let record = order_record(&ctx.state, ClientOrderId::from("O-1")).expect("order record");
        assert_eq!(
            record.filled_qty,
            Decimal::ZERO,
            "mirror must stay unadvanced"
        );
        assert_eq!(record.status, OrderStatus::Submitted);
    }

    #[test]
    fn hostile_wire_order_ids_drop_exec_events_without_panicking() {
        // F7: VenueOrderId::from / ClientOrderId::from panic on empty,
        // whitespace-only, or non-ASCII wire strings (no length cap, unlike
        // TradeId). A server bug or havoc corruption sending such an id must drop
        // the event with a warning, not panic the unsupervised exec task, and
        // must leave the mirror untouched.
        let (ctx, mut rx) = exec_context();

        // Empty venue id on an Accepted: the panicking From would abort here.
        handle_exec_message(
            ServerMessage::OrderAccepted {
                client_order_id: "O-1".into(),
                venue_order_id: String::new(),
                ts_event: 10,
            },
            &ctx,
        );
        assert!(
            rx.try_recv().is_err(),
            "an empty venue id must not emit an accepted event"
        );
        let record = order_record(&ctx.state, ClientOrderId::from("O-1")).expect("order record");
        assert_eq!(
            record.status,
            OrderStatus::Submitted,
            "a dropped accepted must leave the mirror untouched"
        );

        // Empty client id on a fill: guarded at the very top of the fill drain.
        handle_exec_message(
            ServerMessage::OrderFilled(mogwai_protocol::OrderFilled {
                client_order_id: String::new(),
                venue_order_id: "V-1".into(),
                trade_id: "T-1".into(),
                symbol: "BTCUSDT".into(),
                side: Side::Buy,
                last_qty: Decimal::new(1, 0),
                last_px: Decimal::new(10_000, 2),
                leaves_qty: Decimal::ZERO,
                commission: Decimal::ZERO,
                ts_event: 11,
            }),
            &ctx,
        );
        assert!(
            rx.try_recv().is_err(),
            "an empty client id must not emit a fill event"
        );
        let record = order_record(&ctx.state, ClientOrderId::from("O-1")).expect("order record");
        assert_eq!(
            record.filled_qty,
            Decimal::ZERO,
            "a dropped fill must leave the mirror unadvanced"
        );
    }

    #[tokio::test]
    async fn generate_mass_status_composes_the_three_report_sets() {
        // The trait default returns Ok(None), which the live node's startup
        // reconciliation logs as "no mass status available" and reconciles
        // NOTHING. The implementation must compose the three report
        // generators, all three now from venue truth: open orders and fills
        // off the query surface, current positions off GET /account.
        let mut client = execution_client();
        client.instruments = instruments_map();
        // A partial fill keeps the order open, so it passes the open_only
        // filter the canonical mass-status shape applies to order reports.
        let _venue = install_fake_venue(
            &mut client,
            vec![wire_order_info(
                "O-1",
                WireOrderStatus::PartiallyFilled,
                Decimal::new(1, 0),
                10,
            )],
            vec![wire_fill("T-1", Decimal::new(1, 0), 11)],
        );
        let _account =
            install_account_venue(&mut client, vec![wire_position(Decimal::new(1, 0))], 12).await;

        let mass = client
            .generate_mass_status(None)
            .await
            .expect("mass status generates")
            .expect("mass status is Some, not the trait's None default");

        assert_eq!(mass.client_id, client.core.client_id);
        assert_eq!(mass.account_id, client.core.account_id);
        assert_eq!(mass.venue, *MOGWAI_VENUE);
        let orders = mass.order_reports();
        assert_eq!(orders.len(), 1, "the open order must report");
        let order = orders
            .get(&VenueOrderId::from("V-1"))
            .expect("keyed by venue order id");
        assert_eq!(order.order_status, OrderStatus::PartiallyFilled);
        let fills = mass.fill_reports();
        assert_eq!(
            fills.get(&VenueOrderId::from("V-1")).map(Vec::len),
            Some(1),
            "the order's fill must report under its venue id"
        );
        assert_eq!(mass.position_reports().len(), 1, "the position must report");

        // The lookback bounds the FILL reports (still time-filtered by ts_event),
        // but open orders and open positions now report regardless of
        // last-activity time (AE10): a real venue mass-status returns every
        // resting order/position, so a long-quiet open item stamped near the
        // epoch must NOT vanish under a short lookback (which reconciliation
        // would otherwise read as canceled/closed at venue). Only closed and
        // historical records honor the lookback.
        let bounded = client
            .generate_mass_status(Some(1))
            .await
            .expect("bounded mass status generates")
            .expect("bounded mass status is Some");
        assert_eq!(
            bounded.order_reports().len(),
            1,
            "an open order still reports under a short lookback (AE10)"
        );
        assert_eq!(
            bounded.position_reports().len(),
            1,
            "an open position still reports under a short lookback (AE10)"
        );
        assert!(
            bounded.fill_reports().is_empty(),
            "fills remain time-filtered by the lookback"
        );
    }

    #[tokio::test]
    async fn http_post_failure_reject_bypasses_client_drop_havoc() {
        // A synthesized POST-failure reject models a purely local transport
        // failure - it never traveled the wire - so it must NOT pass through
        // the per-dispatch HavocFilter: with drop_prob = 1.0 the filter would
        // discard the terminal event and wedge the order in Submitted forever
        // (the cancel branch already bypasses for the same reason).
        let mut client = execution_client_with_config(MogwaiExecClientConfig {
            transport_profile: TransportProfile::HttpOrders,
            // Loopback port 1: nothing listens there, so the POST fails fast
            // with a transport error instead of waiting out a timeout.
            base_url: "ws://127.0.0.1:1".to_string(),
            havoc: Some(HavocSpec {
                client: ClientHavoc {
                    drop_prob: 1.0,
                    ..ClientHavoc::default()
                },
                ..HavocSpec::default()
            }),
            ..MogwaiExecClientConfig::default()
        });
        let (tx, mut rx) = unbounded_channel();
        client.emitter.set_sender(tx);
        seed_order(&client.state);

        client
            .dispatch_order(ExecWsCommand::Submit(mogwai_protocol::SubmitOrder {
                client_order_id: "O-1".into(),
                symbol: "BTCUSDT".into(),
                side: Side::Buy,
                order_type: mogwai_protocol::OrderType::Limit,
                quantity: Decimal::new(1, 0),
                price: Some(Decimal::new(10_000, 2)),
                time_in_force: mogwai_protocol::TimeInForce::Gtc,
            }))
            .expect("HTTP dispatch spawns");

        let event = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("the synthesized reject must arrive despite drop_prob = 1.0")
            .expect("event channel stays open");
        assert!(matches!(
            event,
            ExecutionEvent::Order(OrderEventAny::Rejected(_))
        ));
        let record = order_record(&client.state, ClientOrderId::from("O-1")).expect("order record");
        assert_eq!(record.status, OrderStatus::Rejected);
    }

    fn order_at(status: OrderStatus, ts: UnixNanos) -> OrderRecord {
        OrderRecord {
            strategy_id: StrategyId::from("S-001"),
            instrument_id: instrument_id(),
            order_side: OrderSide::Buy,
            order_type: OrderType::Limit,
            status,
            quantity: Decimal::new(1, 0),
            price: Some(Decimal::new(10_000, 2)),
            filled_qty: Decimal::ZERO,
            avg_px: None,
            venue_order_id: Some(VenueOrderId::from("V-1")),
            ts_accepted: ts,
            ts_last: ts,
            seen_trades: std::collections::HashSet::new(),
        }
    }

    // AE9: on the WS path, a cancel whose send fails (channel gone: reconnect
    // exhausted or the client stopped) must synthesize a CancelRejected so the
    // order does not sit forever in PendingCancel with no reject to restore it -
    // matching the HTTP path. The mirrored status is left untouched.
    #[test]
    fn ws_cancel_send_failure_synthesizes_cancel_rejected() {
        let mut client = execution_client(); // default = WsStreaming
        let (tx, mut rx) = unbounded_channel();
        client.emitter.set_sender(tx);
        seed_order(&client.state);
        assert!(client.ws_cmd.is_none(), "no live WS channel");

        client
            .dispatch_order(ExecWsCommand::Cancel {
                client_order_id: "O-1".into(),
            })
            .expect("dispatch returns Ok after synthesizing the reject");

        match rx.try_recv().expect("cancel rejected event") {
            ExecutionEvent::Order(OrderEventAny::CancelRejected(event)) => {
                assert_eq!(event.client_order_id, ClientOrderId::from("O-1"));
            }
            other => panic!("expected cancel rejected, got {other:?}"),
        }
        let record = order_record(&client.state, ClientOrderId::from("O-1")).expect("order record");
        assert_eq!(
            record.status,
            OrderStatus::Submitted,
            "a failed WS cancel must leave the mirrored status untouched"
        );
    }

    // AE9: a failed WS modify synthesizes a ModifyRejected (status untouched),
    // and a failed WS submit synthesizes an OrderRejected (Submitted -> Rejected).
    #[test]
    fn ws_modify_and_submit_send_failures_synthesize_rejects() {
        let mut client = execution_client();
        let (tx, mut rx) = unbounded_channel();
        client.emitter.set_sender(tx);
        seed_order(&client.state);

        client
            .dispatch_order(ExecWsCommand::Modify {
                client_order_id: "O-1".into(),
                price: Some(Decimal::new(12_000, 2)),
                quantity: None,
            })
            .expect("modify dispatch returns Ok after synthesizing the reject");
        assert!(matches!(
            rx.try_recv().expect("modify rejected event"),
            ExecutionEvent::Order(OrderEventAny::ModifyRejected(_))
        ));
        assert_eq!(
            order_record(&client.state, ClientOrderId::from("O-1"))
                .expect("order record")
                .status,
            OrderStatus::Submitted,
            "a failed modify leaves the order live"
        );

        client
            .dispatch_order(ExecWsCommand::Submit(mogwai_protocol::SubmitOrder {
                client_order_id: "O-1".into(),
                symbol: "BTCUSDT".into(),
                side: Side::Buy,
                order_type: mogwai_protocol::OrderType::Limit,
                quantity: Decimal::new(1, 0),
                price: Some(Decimal::new(10_000, 2)),
                time_in_force: mogwai_protocol::TimeInForce::Gtc,
            }))
            .expect("submit dispatch returns Ok after synthesizing the reject");
        assert!(matches!(
            rx.try_recv().expect("rejected event"),
            ExecutionEvent::Order(OrderEventAny::Rejected(_))
        ));
        assert_eq!(
            order_record(&client.state, ClientOrderId::from("O-1"))
                .expect("order record")
                .status,
            OrderStatus::Rejected,
            "a failed submit reaches a terminal Rejected state"
        );
    }

    // AE10: an open order under open_only reports regardless of last-activity
    // time (a resting order older than the reconciliation lookback must not be
    // hidden and then inferred canceled-at-venue); the time filter still applies
    // to closed records and whenever open_only is false.
    #[tokio::test]
    async fn open_only_keeps_long_quiet_open_order_but_time_filters_the_rest() {
        let mut client = execution_client();
        client.instruments = instruments_map();
        let _venue = install_fake_venue(
            &mut client,
            vec![
                wire_order_info("O-OPEN", WireOrderStatus::Accepted, Decimal::ZERO, 1),
                wire_order_info("O-CLOSED", WireOrderStatus::Canceled, Decimal::ZERO, 1),
            ],
            Vec::new(),
        );

        let start = Some(UnixNanos::from(1_000_000));
        let open = client
            .generate_order_status_reports(&GenerateOrderStatusReports::new(
                UUID4::new(),
                UnixNanos::from(2_000_000),
                true,
                None,
                start,
                None,
                None,
                None,
            ))
            .await
            .expect("open-only reports");
        assert_eq!(
            open.len(),
            1,
            "the long-quiet open order must still report under open_only (AE10)"
        );
        assert_eq!(open[0].order_status, OrderStatus::Accepted);

        let all = client
            .generate_order_status_reports(&GenerateOrderStatusReports::new(
                UUID4::new(),
                UnixNanos::from(2_000_000),
                false,
                None,
                start,
                None,
                None,
                None,
            ))
            .await
            .expect("all reports");
        assert!(
            all.is_empty(),
            "with open_only false the lookback still filters long-quiet records"
        );
    }

    // AE10 (positions): every position the venue reports is a current open one
    // (the engine drops a symbol the moment it goes flat), so a lookback-bounded
    // start must not hide a long-quiet resting position. The snapshot here is
    // stamped near the epoch, far below the requested `start`.
    #[tokio::test]
    async fn position_report_keeps_long_quiet_open_position() {
        let mut client = execution_client();
        client.instruments = instruments_map();
        let _account =
            install_account_venue(&mut client, vec![wire_position(Decimal::new(1, 0))], 1).await;

        let reports = client
            .generate_position_status_reports(&GeneratePositionStatusReports::new(
                UUID4::new(),
                UnixNanos::from(2_000_000),
                None,
                Some(UnixNanos::from(1_000_000)),
                None,
                None,
                None,
            ))
            .await
            .expect("position reports");
        assert_eq!(
            reports.len(),
            1,
            "a long-quiet open position must still report (AE10)"
        );
    }

    #[tokio::test]
    async fn position_report_filter_matches_the_whole_instrument_id() {
        // The wire rows carry only a symbol, so a symbol-only comparison would
        // hand a request scoped to BTCUSDT on ANOTHER venue this venue's
        // BTCUSDT position - silently ignoring the filter the caller asked for.
        let mut client = execution_client();
        client.instruments = instruments_map();
        let _account =
            install_account_venue(&mut client, vec![wire_position(Decimal::new(1, 0))], 12).await;

        let foreign = InstrumentId::new(
            nautilus_model::identifiers::Symbol::from("BTCUSDT"),
            Venue::from("BINANCE"),
        );
        let reports = client
            .generate_position_status_reports(&GeneratePositionStatusReports::new(
                UUID4::new(),
                UnixNanos::from(20),
                Some(foreign),
                None,
                None,
                None,
                None,
            ))
            .await
            .expect("position reports");
        assert!(
            reports.is_empty(),
            "a same-symbol, different-venue filter must match nothing here"
        );
    }

    #[tokio::test]
    async fn position_reports_survive_a_server_without_the_account_route() {
        // `connect` treats a 404 as a server predating GET /account and carries
        // on; this generator must agree. Failing here would take the whole mass
        // status down - orders and fills included - over a route the server was
        // never expected to have.
        let mut client = execution_client();
        client.instruments = instruments_map();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind 404 venue");
        let port = listener.local_addr().expect("addr").port();
        client.config.base_url = format!("ws://127.0.0.1:{port}");
        let _venue = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0_u8; 1024];
                drop(stream.read(&mut buf).await);
                drop(
                    stream
                        .write_all(
                            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .await,
                );
            }
        });

        let reports = client
            .generate_position_status_reports(&GeneratePositionStatusReports::new(
                UUID4::new(),
                UnixNanos::from(20),
                None,
                None,
                None,
                None,
                None,
            ))
            .await
            .expect("a 404 must not fail the generator");
        assert!(reports.is_empty(), "a blind generator reports nothing");
    }

    // AE6: ExecState.prune bounds the terminal-order records past their cap
    // (oldest-first) while never dropping an open order.
    #[test]
    fn exec_state_prune_bounds_terminal_orders_keeping_open() {
        let mut state = ExecState::default();
        for i in 0..(MAX_TERMINAL_ORDERS as u64 + 2) {
            state.orders.insert(
                ClientOrderId::from(format!("C-{i}").as_str()),
                order_at(OrderStatus::Canceled, UnixNanos::from(i)),
            );
        }
        state.orders.insert(
            ClientOrderId::from("OPEN"),
            order_at(OrderStatus::Accepted, UnixNanos::from(5)),
        );

        state.prune();

        let terminal = state
            .orders
            .values()
            .filter(|record| record.status.is_closed())
            .count();
        assert_eq!(
            terminal, MAX_TERMINAL_ORDERS,
            "terminal orders capped at MAX_TERMINAL_ORDERS"
        );
        assert!(
            state.orders.contains_key(&ClientOrderId::from("OPEN")),
            "an open order is never pruned"
        );
    }
}
