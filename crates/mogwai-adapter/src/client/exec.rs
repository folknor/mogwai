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
        atomic::{AtomicBool, Ordering},
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
    enums::{
        ContingencyType, LiquiditySide, OmsType, OrderSide, OrderStatus, OrderType,
        PositionSideSpecified, TriggerType,
    },
    events::{
        AccountState as NautilusAccountState, OrderAccepted, OrderCancelRejected, OrderCanceled,
        OrderEventAny, OrderFilled, OrderModifyRejected, OrderRejected, OrderSubmitted,
        OrderTriggered, OrderUpdated,
    },
    identifiers::{
        AccountId, ClientId, ClientOrderId, InstrumentId, PositionId, TradeId, Venue, VenueOrderId,
    },
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
        HavocDelivery, HavocFilter, abort_tasks, client_havoc, conn_havoc, enqueue_havoc,
        fetch_clock_or_identity, flush_havoc_into_pump, instrument_def, join_url, lock_recover,
        now_unix_nanos, request_timeout_secs, run_identity_check, seed_instruments,
        spawn_latency_pump, symbol_from_instrument, track_task, wait_connected,
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

/// Notes the venue's account label when it differs from the configured one.
///
/// This was a FATAL equality check, and killing it is the point. Under the
/// retired per-account model a client named a slot and the venue honoured it, so
/// a snapshot bearing another id meant the venue had routed someone else's
/// account here and adopting it would have corrupted this one. One venue is now
/// one run is one LEDGER: the connection carries the only account there is,
/// there is nothing to be misrouted from, and the venue's id is a label rather
/// than a key. An equality check is a per-slot invariant that outlived the
/// slots.
///
/// It also could not be satisfied. The venue reported a bare `MOGWAI` for one
/// release - legal as a `mogwai_protocol::AccountId`, and unconstructable as a
/// nautilus one, which parses `ISSUER-NUMBER` - so no configured value could
/// equal it and every run died on connect. That specific id is fixed venue-side,
/// but a check that can deadlock a run over a label is worth removing on its own
/// terms rather than repairing.
///
/// Differing is still worth SAYING once, at connect: it means the venue config
/// and the client config name the account differently, which is confusing when
/// reading two logs side by side even though nothing downstream depends on it.
fn note_account_label(state: &mogwai_protocol::AccountState, configured: AccountId) {
    if state.account_id.as_str() != configured.as_ref() {
        tracing::info!(
            venue_account = %state.account_id.as_str(),
            configured_account = %configured,
            "the venue labels its ledger differently from this client; using the configured id"
        );
    }
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
            HashMap::new(),
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
            task_handles: Arc::new(Mutex::new(Vec::new())),
        })
    }

    /// Retires the current transport generation's connectivity flag by
    /// REPLACING the shared cell, not by storing `false` into it.
    ///
    /// `abort_tasks` is not synchronous: cancellation is delivered at the
    /// aborted task's next await point, so a reader caught between
    /// `connect_async(..).await` returning and its first select can still run
    /// `connected.store(true)` AFTER the caller has stored `false`. That
    /// leaves a stopped client reporting itself connected, and a subsequent
    /// `wait_connected` returning success for a socket that never opened.
    /// Swapping the `Arc` means the retired reader writes to a cell nobody
    /// reads: the drop timing of the old generation stops being a
    /// correctness property.
    fn retire_connected_flag(&mut self) {
        self.connected = Arc::new(AtomicBool::new(false));
    }

    /// The status this client's reconciliation mirror currently holds for one
    /// order, or `None` if the mirror does not know it.
    ///
    /// Read-only, and deliberately the mirror rather than venue truth: the
    /// mirror is the adapter's own belief about an order, and a class of defect
    /// exists that is invisible from every other surface - an amend ack that
    /// recomputes status from fill progress alone walks a TRIGGERED conditional
    /// back to `Accepted`, desyncing the mirror from the engine for the rest of
    /// the run without changing a single emitted event. Nautilus' own FSM keeps
    /// `(Triggered, Updated) => Triggered`. Exposed so that invariant can be
    /// gated; it is never a control surface.
    #[must_use]
    pub fn mirrored_order_status(&self, client_order_id: ClientOrderId) -> Option<OrderStatus> {
        order_record(&self.state, client_order_id).map(|record| record.status)
    }

    #[cfg(all(test, any()))]
    fn is_started(&self) -> bool {
        self.core.is_started()
    }

    #[cfg(all(test, any()))]
    fn is_stopped(&self) -> bool {
        self.core.is_stopped()
    }

    fn send_ws(&self, cmd: &ExecWsCommand) -> anyhow::Result<()> {
        let tx = self
            .ws_cmd
            .as_ref()
            .context("mogwai execution client is not connected")?;
        tx.send(cmd.clone())
            .context("send execution websocket command")
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

    fn dispatch_order(&self, cmd: &ExecWsCommand) -> anyhow::Result<()> {
        if let Err(err) = self.send_ws(cmd) {
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
            synthesize_transport_reject(cmd, &err, &ctx);
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
        VenueQuery {
            ws_cmd: self.ws_cmd.clone(),
            pending: Arc::clone(&self.pending),
            timeout_secs: request_timeout_secs(&self.config.havoc, self.sim),
        }
    }
}

/// The websocket request/reply transport for venue-truth queries. A query is
/// correlated to its reply through `pending` by the echoed `request_id`, since
/// the reply shares the socket with unsolicited execution events.
///
/// Delivery is havoc-able, content is not: the reply is always a truthful
/// engine book read, but on the WS carrier it classifies as execution, so an
/// armed `DelayAcks` holds it and `GoDark` drops it - a lost or late reply
/// surfaces here as a timeout, exercising the consumer's query-timeout path
/// without ever letting havoc alter what the venue says.
#[derive(Clone)]
struct VenueQuery {
    ws_cmd: Option<UnboundedSender<ExecWsCommand>>,
    pending: Arc<Mutex<PendingQueries>>,
    timeout_secs: u64,
}

impl VenueQuery {
    async fn order_status(
        &self,
        client_order_id: Option<String>,
        open_only: bool,
    ) -> anyhow::Result<OrderStatusSnapshot> {
        let request_id = UUID4::new().to_string();
        let reply_rx = self.register_ws_query(
            |pending, tx| drop(pending.orders.insert(request_id.clone(), tx)),
            |pending| drop(pending.orders.remove(&request_id)),
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
        let reply_rx = self.register_ws_query(
            |pending, tx| drop(pending.fills.insert(request_id.clone(), tx)),
            |pending| drop(pending.fills.remove(&request_id)),
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
        unregister: impl FnOnce(&mut PendingQueries),
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
        if tx.send(cmd).is_err() {
            unregister(&mut lock_recover(&self.pending, "pending queries"));
            anyhow::bail!("send execution websocket query command: channel closed");
        }
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
    let instrument_id = convert::instrument_id(&def)
        .map_err(|err| {
            tracing::warn!(
                order = %info.client_order_id,
                error = %err,
                "dropping order status report: unrepresentable instrument symbol"
            );
        })
        .ok()?;
    let mut report = OrderStatusReport::new(
        account_id,
        instrument_id,
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
    );
    let convert_px = |value, label| {
        convert::price(value, def.price_precision)
            .map_err(|err| {
                tracing::warn!(
                    order = %info.client_order_id,
                    field = label,
                    error = %err,
                    "dropping order status report: unrepresentable price"
                );
            })
            .ok()
    };
    // A stop-limit report without its limit price is unreconcilable, and one
    // without its trigger price says nothing about what the venue is waiting
    // for - both are set here rather than left at the constructor's `None`.
    if let Some(value) = info.price {
        report = report.with_price(convert_px(value, "price")?);
    }
    if let Some(value) = info.trigger_price {
        // The venue has one trigger reference and only one: the last trade.
        report = report
            .with_trigger_price(convert_px(value, "trigger_price")?)
            .with_trigger_type(TriggerType::LastPrice);
    }
    if let Some(ts) = info.ts_triggered {
        report = report.with_ts_triggered(UnixNanos::from(ts));
    }
    if let Some(position_id) = &info.position_id {
        let position_id = PositionId::new_checked(position_id)
            .map_err(|err| {
                tracing::warn!(
                    order = %info.client_order_id,
                    field = "position_id",
                    error = %err,
                    "dropping order status report: unrepresentable venue position id"
                );
            })
            .ok()?;
        report = report.with_venue_position_id(position_id);
    }
    Some(
        report
            .with_reduce_only(info.reduce_only)
            .with_post_only(info.post_only),
    )
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
    let commission_currency = match Currency::from_str(&fill.commission_currency) {
        Ok(currency) => currency,
        Err(err) => {
            warn_drop("commission_currency", &err);
            return None;
        }
    };
    let commission = match convert::money(fill.commission, commission_currency) {
        Ok(commission) => commission,
        Err(err) => {
            warn_drop("commission", &err);
            return None;
        }
    };
    let position_id = fill
        .position_id
        .as_deref()
        .and_then(|id| PositionId::new_checked(id).ok());
    let instrument_id = match convert::instrument_id(&def) {
        Ok(id) => id,
        Err(err) => {
            warn_drop("symbol", &err);
            return None;
        }
    };
    // Taker unconditionally: the wire carries no maker/taker flag (see the
    // fill event handler's identical, deliberately lossy mapping).
    Some(FillReport::new(
        account_id,
        instrument_id,
        venue_order_id,
        trade_id,
        convert::nautilus_side(fill.side),
        last_qty,
        last_px,
        commission,
        match fill.liquidity_side {
            mogwai_protocol::LiquiditySide::Maker => LiquiditySide::Maker,
            mogwai_protocol::LiquiditySide::Taker => LiquiditySide::Taker,
        },
        Some(client_order_id),
        position_id,
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
        self.ws_cmd = None;
        abort_tasks(&self.task_handles);
        self.retire_connected_flag();
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
        // A client owns exactly one transport generation. This cleanup is
        // unconditional and independent of the component started flag: hosts
        // may connect without calling start, and reconnect after stop.
        self.ws_cmd = None;
        abort_tasks(&self.task_handles);
        self.retire_connected_flag();
        {
            let mut pending = lock_recover(&self.pending, "pending queries");
            pending.orders.clear();
            pending.fills.clear();
        }
        let http_base_url = self.config.http_base_url();
        // The execution client rides only the affine map; the tape boundary in
        // the envelope is the data client's concern.
        let sim = fetch_clock_or_identity(&self.http, &http_base_url)
            .await
            .0
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
        // reconnect therefore stays lost until the next snapshot any order
        // transition pushes,
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
                note_account_label(&state, self.config.account_id);
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

        // `ws_url` already carries the `/ws` path.
        let ws_url = self.config.ws_url();
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
        let identity = run_identity_check(
            self.http.clone(),
            self.http_quota.clone(),
            http_base_url.clone(),
            self.config.expected_run_seed,
        );
        let reader_handle = tokio::spawn(async move {
            run_ws_connection(
                WsConnectionConfig {
                    ws_url: task_ws_url,
                    conn,
                    seed: client_havoc.seed,
                    connected,
                    sim,
                    label: "exec",
                    identity,
                },
                Some(cmd_rx),
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
            abort_tasks(&self.task_handles);
            self.retire_connected_flag();
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
        convert::wire_trigger_type(cmd.order_init.trigger_type)?;
        if cmd
            .order_init
            .trigger_instrument_id
            .is_some_and(|id| id != cmd.instrument_id)
        {
            anyhow::bail!(
                "cross-instrument triggers are unsupported: MOGWAI triggers from the order instrument's tape"
            );
        }
        if cmd.order_init.order_list_id.is_some()
            || cmd
                .order_init
                .linked_order_ids
                .as_ref()
                .is_some_and(|ids| !ids.is_empty())
        {
            anyhow::bail!(
                "order lists and linked bracket legs are unsupported: place and cancel fixed stops from the strategy"
            );
        }
        if cmd
            .order_init
            .contingency_type
            .is_some_and(|kind| kind != ContingencyType::NoContingency)
        {
            anyhow::bail!(
                "contingent order lists are unsupported: place and cancel fixed stops from the strategy"
            );
        }
        let wire = mogwai_protocol::SubmitOrder {
            client_order_id: cmd.client_order_id.to_string(),
            symbol: symbol_from_instrument(cmd.instrument_id),
            position_id: cmd.position_id.map(|id| id.to_string()),
            side: convert::wire_side(cmd.order_init.order_side)?,
            order_type: convert::wire_order_type(cmd.order_init.order_type)?,
            quantity: cmd.order_init.quantity.as_decimal(),
            price: cmd.order_init.price.map(|p| p.as_decimal()),
            trigger_price: cmd.order_init.trigger_price.map(|p| p.as_decimal()),
            reduce_only: cmd.order_init.reduce_only,
            post_only: cmd.order_init.post_only,
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
                    venue_order_id: None,
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

        self.dispatch_order(&ExecWsCommand::Submit(wire))
    }

    fn modify_order(&self, cmd: ModifyOrder) -> anyhow::Result<()> {
        self.dispatch_order(&ExecWsCommand::Modify {
            client_order_id: cmd.client_order_id.to_string(),
            price: cmd.price.map(|p| p.as_decimal()),
            quantity: cmd.quantity.map(|q| q.as_decimal()),
            trigger_price: cmd.trigger_price.map(|p| p.as_decimal()),
        })
    }

    fn cancel_order(&self, cmd: CancelOrder) -> anyhow::Result<()> {
        self.dispatch_order(&ExecWsCommand::Cancel {
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
        note_account_label(&state, self.config.account_id);
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
                let instrument_id = convert::instrument_id(&def)
                    .map_err(|err| {
                        tracing::warn!(
                            symbol = %position.symbol,
                            error = %err,
                            "dropping position report: unrepresentable instrument symbol"
                        );
                    })
                    .ok()?;
                Some(PositionStatusReport::new(
                    self.core.account_id,
                    instrument_id,
                    position_side(position.quantity),
                    quantity,
                    ts_event,
                    ts_init,
                    None,
                    position
                        .position_id
                        .as_deref()
                        .and_then(|id| PositionId::new_checked(id).ok()),
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
        let open_venue_order_ids: std::collections::HashSet<_> = order_reports
            .iter()
            .map(|report| report.venue_order_id)
            .collect();
        // The lookback is asked UNBOUNDED and narrowed here rather than being
        // pushed into the generator, for two reasons. An order report carries
        // no `avg_px`, so a partially filled OPEN order can only be paired from
        // its own fills, including the ones older than the lookback - dropping
        // those makes the host reconcile a wrong average price. And a single
        // pass keeps every fill of an order in the venue's own chronological
        // order; appending the older ones after the recent ones would hand
        // nautilus a group it has to re-sort.
        let fill_reports: Vec<_> = self
            .generate_fill_reports(GenerateFillReports::new(
                UUID4::new(),
                ts_init,
                None,
                None,
                None,
                None,
                None,
                None,
            ))
            .await?
            .into_iter()
            .filter(|report| {
                open_venue_order_ids.contains(&report.venue_order_id)
                    || in_time_range(report.ts_event, start, None)
            })
            .collect();
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
        trigger_price: Option<Decimal>,
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
            trigger_price,
        } => ClientMessage::ModifyOrder {
            client_order_id,
            price,
            quantity,
            trigger_price,
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
    venue_order_id: Option<VenueOrderId>,
    ts_last: UnixNanos,
    /// `trade_id`s already applied to this order's reconciliation mirror. The
    /// duplicate-fill divergence (`DuplicateNextFill`) and client-side
    /// `duplicate_prob` deliberately deliver the same `OrderFilled` twice; the
    /// duplicate wire event is forwarded downstream (the intended divergence),
    /// but it must not double-apply to the mirror, so the second sighting of a
    /// `trade_id` skips the mirror status mutation.
    seen_trades: std::collections::HashSet<TradeId>,
}

fn handle_exec_message(msg: ServerMessage, ctx: &ExecContext) {
    match msg {
        ServerMessage::OrderTriggered { client_order_id, venue_order_id, ts_event } => {
            let Some(client_order_id) = wire_client_order_id(&client_order_id) else { return; };
            let Some(venue_order_id) = wire_venue_order_id(&venue_order_id) else { return; };
            let Some((record, stale)) = with_order_record(&ctx.state, client_order_id, |record| {
                let stale = record.status.is_closed();
                if !stale { record.status = OrderStatus::Triggered; record.ts_last = UnixNanos::from(ts_event); }
                (record.clone(), stale)
            }) else { return; };
            if stale { tracing::warn!(%client_order_id, "dropping trigger for terminal order"); return; }
            let event = OrderTriggered::new(ctx.trader_id, record.strategy_id, record.instrument_id,
                client_order_id, UUID4::new(), UnixNanos::from(ts_event), now_unix_nanos(ctx.sim),
                false, Some(venue_order_id), Some(ctx.account_id));
            ctx.emitter.send_order_event(OrderEventAny::Triggered(event));
        }
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
            trigger_price,
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
            let updated_trigger_price = match trigger_price
                .map(|p| convert::price(p, def.price_precision))
                .transpose()
            {
                Ok(price) => price,
                Err(err) => {
                    tracing::warn!(order = %client_order_id, error = %err, "dropping order update: unrepresentable trigger price");
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
                    let filled_qty = (quantity - leaves_qty).max(Decimal::ZERO);
                    // An amend never reverses an in-progress fill: an order with a
                    // non-zero filled_qty stays PARTIALLY_FILLED (or flips to FILLED
                    // when the amend leaves nothing outstanding) so the mirror does
                    // not report Accepted alongside a non-zero filled_qty.
                    record.status = if filled_qty.is_zero() {
                        if record.status == OrderStatus::Triggered { OrderStatus::Triggered } else { OrderStatus::Accepted }
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
                updated_trigger_price,
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
                // A whole outbound BATCH the venue discarded, so the events it
                // carried are simply gone. Same severity and same wording as
                // the FeedLagged arm below, and for the same reason: the
                // mirror may now disagree with venue truth and only a host-
                // driven reconciliation can settle it. Nothing here can
                // trigger that; see the cross-repo item in notes/todo.md.
                tracing::error!(
                    ?subject,
                    %reason,
                    "venue refused a whole execution frame; order events may be missing and the mirror should be reconciled against venue truth"
                );
            }
        },
        ServerMessage::FeedLagged {
            skipped,
            sim_now_ns,
        } => {
            // ERROR for the same reason as the data client's arm: a drop on
            // the EXECUTION socket can have taken order events with it, so
            // the nautilus order state may now disagree with venue truth.
            // Recovery exists - the venue-truth report generators re-derive
            // the order, fill and position sets - but it cannot be triggered
            // FROM HERE, and that is structural rather than an omission: this
            // translator runs as `handler(msg).await` inside the reader's own
            // frame loop, so a venue-truth query issued here would wait for a
            // reply only this blocked loop can read, and the client is `!Send`
            // (it owns an `Rc<RefCell<Cache>>` through `ExecutionClientCore`)
            // so the query cannot be spawned off either. Rebuilding a report
            // from the local mirror would assert as truth the very frames the
            // venue just said it dropped. The log is therefore what tells a
            // host to reconcile; the upstream half is in notes/todo.md.
            tracing::error!(
                skipped,
                sim_now_ns,
                "venue dropped frames on the execution socket; order events may be missing and the mirror should be reconciled against venue truth"
            );
        }
        // Market data is handled by the data client.
        ServerMessage::Trade(_)
        | ServerMessage::Quote(_)
        | ServerMessage::HavocDiagnostic { .. }
        // A clean venue completion is a transport concern.  The reader owns
        // reconnect policy; the execution event translator has no event to
        // publish for it.
        | ServerMessage::RunComplete { .. } => {}
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
    let settlement_currency = def.class.settlement_currency();
    let Ok(quote_currency) = Currency::from_str(settlement_currency) else {
        tracing::warn!(
            symbol = %fill.symbol,
            quote = %settlement_currency,
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
    // but the reconciliation mirror must apply each economic fill exactly once,
    // so the second sighting skips the mutation.
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
            // but must not regress the terminal status back to PartiallyFilled
            // and re-open a closed order in the reconciliation mirror.
            if !record.status.is_closed() {
                record.status = if fill.leaves_qty.is_zero() {
                    OrderStatus::Filled
                } else {
                    OrderStatus::PartiallyFilled
                };
            }
            record.venue_order_id = Some(venue_order_id);
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
        let commission_currency =
            Currency::from_str(&fill.commission_currency).unwrap_or(quote_currency);
        match convert::money(fill.commission, commission_currency) {
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
    let position_id = fill
        .position_id
        .as_deref()
        .and_then(|id| PositionId::new_checked(id).ok());
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
        match fill.liquidity_side {
            mogwai_protocol::LiquiditySide::Maker => LiquiditySide::Maker,
            mogwai_protocol::LiquiditySide::Taker => LiquiditySide::Taker,
        },
        UUID4::new(),
        UnixNanos::from(fill.ts_event),
        now_unix_nanos(ctx.sim),
        false,
        position_id,
        commission,
        None,
    );
    ctx.emitter.send_order_event(OrderEventAny::Filled(event));
}

fn handle_account_state(state: &mogwai_protocol::AccountState, ctx: &ExecContext) {
    // The wire's account id is deliberately NOT compared against the configured
    // one here, and the difference matters more on this path than on the connect
    // path: this used to DROP a snapshot whose label differed, so a venue and a
    // client that named the account differently produced a client whose balances
    // silently stopped updating while every fill still arrived.
    //
    // The drop guarded against adopting a MISROUTED snapshot back when a venue
    // served many accounts and could route one to the wrong session. This venue
    // serves one run with one ledger, so the only account there is, is the one
    // this connection carries - there is nothing to be misrouted from, and a
    // dropped snapshot can only lose state that was correct. The configured id
    // is stamped on below, as it always was; `note_account_label` says once at
    // connect if the two names differ.
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
    let instruments = lock_recover(&ctx.instruments, "instrument definitions");
    let margins = state
        .margins
        .iter()
        .filter_map(|margin| {
            let currency = Currency::from_str(&margin.currency).ok()?;
            let initial = convert::money(margin.initial, currency).ok()?;
            let maintenance = convert::money(margin.maintenance, currency).ok()?;
            // A margin row whose symbol cannot form an instrument id keeps the
            // balance and drops only the attribution, which is what the
            // absent-def case already does.
            let instrument_id = instruments
                .get(&margin.symbol)
                .and_then(|def| convert::instrument_id(def).ok());
            match MarginBalance::new_checked(initial, maintenance, instrument_id) {
                Ok(balance) => Some(balance),
                Err(err) => {
                    tracing::warn!(symbol = %margin.symbol, error = %err, "dropping invalid margin balance");
                    None
                }
            }
        })
        .collect();
    drop(instruments);

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
        margins,
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
    use super::*;

    #[test]
    fn a_failed_query_send_unregisters_its_waiter() {
        let (ws_tx, ws_rx) = unbounded_channel();
        drop(ws_rx);
        let pending = Arc::new(Mutex::new(PendingQueries::default()));
        let query = VenueQuery {
            ws_cmd: Some(ws_tx),
            pending: Arc::clone(&pending),
            timeout_secs: 1,
        };
        let request_id = "Q-1".to_string();

        let result = query.register_ws_query::<OrderStatusSnapshot>(
            |pending, tx| drop(pending.orders.insert(request_id.clone(), tx)),
            |pending| drop(pending.orders.remove(&request_id)),
            ExecWsCommand::QueryOrders {
                request_id: request_id.clone(),
                client_order_id: None,
                open_only: false,
            },
        );

        assert!(result.is_err());
        assert!(
            lock_recover(&pending, "pending queries").orders.is_empty(),
            "send failure must not retain a waiter"
        );
    }
}
