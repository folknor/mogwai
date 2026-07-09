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
use mogwai_protocol::{ClientMessage, HavocSpec, InstrumentDef, ServerMessage, SimClock, Symbol};
use nautilus_common::{
    clients::ExecutionClient,
    live::{get_runtime, try_get_exec_event_sender},
    messages::execution::{
        CancelOrder, GenerateFillReports, GenerateOrderStatusReports,
        GeneratePositionStatusReports, ModifyOrder, SubmitOrder,
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
        HavocFilter, abort_tasks, client_havoc, client_havoc_for_dispatch, conn_havoc,
        dispatch_havoc, fetch_clock_or_identity, flush_havoc, instrument_def, join_url,
        lock_recover, now_unix_nanos, request_timeout_secs, seed_instruments,
        symbol_from_instrument, track_task, wait_connected,
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
        self.core.set_stopped();
        self.core.set_disconnected();
        Ok(())
    }

    fn reset(&mut self) -> anyhow::Result<()> {
        // Mirror MogwaiDataClient::reset: stop first (abort tasks, drop the WS
        // command channel), then clear the reconciliation mirror. Without this
        // the default no-op `reset` leaves ExecState.orders/fills/positions
        // populated across a stop/start, so a prior session's orders leak into
        // the next session's status/fill/position reports.
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
        // Failure policy: a 404 means a server predating GET /account; warn and
        // fall back to the legacy reactive path (the account seeds off the first
        // fill, as before this fix). Any OTHER failure against a server that does
        // publish the route is fatal - warn-and-continue there would silently
        // recreate the exact first-fill cache-miss this fix exists to eliminate.
        match fetch_account(&self.http, &self.http_quota, &http_base_url).await {
            Ok(state) => {
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
        let ctx = self.exec_context();
        let havoc_filter = Arc::new(tokio::sync::Mutex::new(HavocFilter::from_client(
            &client_havoc,
        )));
        let handler_filter = Arc::clone(&havoc_filter);
        let disconnect_filter = Arc::clone(&havoc_filter);
        let handler_ctx = ctx.clone();
        let disconnect_ctx = ctx;
        let task_ws_url = ws_url.clone();
        let reader_handle = tokio::spawn(async move {
            run_ws_connection(
                WsConnectionConfig {
                    ws_url: task_ws_url,
                    conn,
                    seed: client_havoc.seed,
                    connected,
                    sim,
                },
                cmd_rx,
                exec_command_to_client_message,
                Vec::new,
                move |server_msg| {
                    let handler_filter = Arc::clone(&handler_filter);
                    let ctx = handler_ctx.clone();
                    async move {
                        let mut filter = handler_filter.lock().await;
                        dispatch_havoc(&mut filter, server_msg, sim, |msg| async {
                            handle_exec_message(msg, &ctx);
                        })
                        .await;
                    }
                },
                move || {
                    let disconnect_filter = Arc::clone(&disconnect_filter);
                    let ctx = disconnect_ctx.clone();
                    async move {
                        let mut filter = disconnect_filter.lock().await;
                        flush_havoc(&mut filter, sim, |msg| async {
                            handle_exec_message(msg, &ctx);
                        })
                        .await;
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
                    time_in_force: cmd.order_init.time_in_force,
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

    async fn generate_order_status_reports(
        &self,
        cmd: &GenerateOrderStatusReports,
    ) -> anyhow::Result<Vec<OrderStatusReport>> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("execution state mutex poisoned"))?;
        let reports = state
            .orders
            .iter()
            .filter(|(_, record)| !cmd.open_only || record.status.is_open())
            .filter(|(_, record)| {
                cmd.instrument_id
                    .is_none_or(|id| id == record.instrument_id)
            })
            .filter(|(_, record)| {
                // An open order requested under open_only is always included,
                // regardless of when it last had activity: a real venue
                // mass-status returns every resting order, and reconciliation
                // passes a lookback-bounded `start`, so filtering a long-quiet
                // open order by `ts_last` used to hide it - and the manager then
                // infers it canceled-at-venue (AE10). The time filter still
                // applies to closed/historical records (open_only false).
                (cmd.open_only && record.status.is_open())
                    || in_time_range(record.ts_last, cmd.start, cmd.end)
            })
            .filter_map(|(client_order_id, record)| {
                // A Submitted order with no venue ack yet carries no
                // venue_order_id. `VenueOrderId::from("")` routes through the
                // panicking `VenueOrderId::new`, which nautilus's own
                // `#[should_panic]` empty-string test confirms rejects the
                // empty string - so a `None` cannot be papered over with a
                // placeholder here. There is also nothing venue-side to
                // reconcile yet for an order the venue has not acknowledged,
                // so the record is dropped from this report set (with a
                // warning) rather than risk a bogus/duplicate venue id
                // corrupting the reconciliation manager's venue-order-id
                // index.
                let venue_order_id = record.venue_order_id.or_else(|| {
                    tracing::warn!(
                        order = %client_order_id,
                        status = ?record.status,
                        "omitting order status report: no venue order id yet (unacked submit)"
                    );
                    None
                })?;
                // A record whose mirrored quantity/filled_qty cannot represent
                // as a nautilus Quantity (hostile magnitude/precision off the
                // wire) is dropped from the report set with a warning rather
                // than panicking the report generator.
                let quantity = record
                    .quantity_for_report(&self.instruments)
                    .map_err(|err| {
                        tracing::warn!(
                            order = %client_order_id,
                            error = %err,
                            "dropping order status report: unrepresentable quantity"
                        );
                    })
                    .ok()?;
                let filled = record
                    .filled_quantity_for_report(&self.instruments)
                    .map_err(|err| {
                        tracing::warn!(
                            order = %client_order_id,
                            error = %err,
                            "dropping order status report: unrepresentable filled quantity"
                        );
                    })
                    .ok()?;
                Some(OrderStatusReport::new(
                    self.core.account_id,
                    record.instrument_id,
                    Some(*client_order_id),
                    venue_order_id,
                    record.order_side,
                    record.order_type,
                    record.time_in_force,
                    record.status,
                    quantity,
                    filled,
                    record.ts_accepted,
                    record.ts_last,
                    now_unix_nanos(self.sim),
                    None,
                ))
            })
            .collect();
        Ok(reports)
    }

    async fn generate_fill_reports(
        &self,
        cmd: GenerateFillReports,
    ) -> anyhow::Result<Vec<FillReport>> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("execution state mutex poisoned"))?;
        let reports = state
            .fills
            .iter()
            .filter(|fill| cmd.instrument_id.is_none_or(|id| id == fill.instrument_id))
            .filter(|fill| {
                cmd.venue_order_id
                    .is_none_or(|id| id == fill.venue_order_id)
            })
            .filter(|fill| in_time_range(fill.ts_event, cmd.start, cmd.end))
            .filter_map(|fill| {
                // A commission that cannot represent as nautilus Money drops
                // just this fill report with a warning; the rest still report.
                let commission = convert::money(fill.commission, fill.quote_currency)
                    .map_err(|err| {
                        tracing::warn!(
                            trade = %fill.trade_id,
                            error = %err,
                            "dropping fill report: unrepresentable commission"
                        );
                    })
                    .ok()?;
                Some(FillReport::new(
                    self.core.account_id,
                    fill.instrument_id,
                    fill.venue_order_id,
                    fill.trade_id,
                    fill.order_side,
                    fill.last_qty,
                    fill.last_px,
                    commission,
                    LiquiditySide::Taker,
                    Some(fill.client_order_id),
                    None,
                    fill.ts_event,
                    now_unix_nanos(self.sim),
                    None,
                ))
            })
            .collect();
        Ok(reports)
    }

    async fn generate_position_status_reports(
        &self,
        cmd: &GeneratePositionStatusReports,
    ) -> anyhow::Result<Vec<PositionStatusReport>> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("execution state mutex poisoned"))?;
        let reports = state
            .positions
            .values()
            .filter(|position| {
                cmd.instrument_id
                    .is_none_or(|id| id == position.instrument_id)
            })
            .filter(|position| {
                // Every position the mirror holds is a current open (nonzero)
                // venue position - the account-snapshot apply drops flat
                // instruments - so a lookback-bounded `start` must not hide a
                // long-quiet resting position (reconciliation would otherwise
                // have to re-adopt it as EXTERNAL mid-run) (AE10). Include any
                // open position regardless of last-activity time; the time filter
                // still applies to a defensive flat entry.
                !position.quantity.is_zero() || in_time_range(position.ts_last, cmd.start, cmd.end)
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
                    position.instrument_id,
                    position_side(position.quantity),
                    quantity,
                    position.ts_last,
                    now_unix_nanos(self.sim),
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
    trader_id: nautilus_model::identifiers::TraderId,
    account_id: AccountId,
    account_type: nautilus_model::enums::AccountType,
    sim: SimClock,
}

#[derive(Debug, Default)]
struct ExecState {
    orders: HashMap<ClientOrderId, OrderRecord>,
    fills: Vec<FillRecord>,
    positions: HashMap<Symbol, PositionRecord>,
    /// `ts_event` of the last account snapshot applied to the position mirror.
    /// Snapshots carry the venue's COMPLETE non-zero position set and are
    /// applied destructively (absent symbols are dropped as flat), which is
    /// only sound if each applied snapshot is at least as new as the last:
    /// reorder/duplicate havoc delivering an OLDER snapshot would resurrect a
    /// just-closed position (the phantom-EXTERNAL desync class the drop rule
    /// exists to kill), erase a just-opened one, and move ts_last backward.
    /// `handle_account_state` skips any snapshot below this watermark.
    account_ts_last: UnixNanos,
}

/// Cap on retained terminal order records. Open orders are never pruned (they
/// are live reconciliation truth); only closed records beyond this many are
/// dropped, oldest-by-`ts_last` first, so a report over the retained window
/// keeps every record it needs while a long forward run cannot accumulate
/// terminal orders without bound (AE6).
const MAX_TERMINAL_ORDERS: usize = 10_000;

/// Cap on the append-only `fills` Vec, pruned oldest-first past this bound.
/// Fill reports are lookback-bounded, so the oldest fills beyond this many are
/// never needed by a report within the retained window (AE6).
const MAX_FILLS: usize = 10_000;

impl ExecState {
    /// Bounds the mirror's memory: an unpruned `orders` map (terminal records
    /// and permanently-Submitted strays live forever) and an append-only `fills`
    /// Vec otherwise grow linearly over a long forward run, and every report
    /// generation scans them all. Prunes the oldest terminal orders and the
    /// oldest fills past their caps; open orders are always retained. Called
    /// after each mirror mutation that can grow the maps (a submit insert, a fill
    /// push), and does real work only when a cap is exceeded.
    fn prune(&mut self) {
        if self.fills.len() > MAX_FILLS {
            // `fills` is appended in arrival order (ascending ts on the clean
            // path), so draining the front drops the oldest.
            let excess = self.fills.len() - MAX_FILLS;
            self.fills.drain(0..excess);
        }
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
    time_in_force: nautilus_model::enums::TimeInForce,
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

impl OrderRecord {
    fn quantity_for_report(
        &self,
        instruments: &Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    ) -> anyhow::Result<nautilus_model::types::Quantity> {
        let precision = report_size_precision(instruments, self.instrument_id)?;
        convert::quantity(self.quantity, precision)
    }

    fn filled_quantity_for_report(
        &self,
        instruments: &Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    ) -> anyhow::Result<nautilus_model::types::Quantity> {
        let precision = report_size_precision(instruments, self.instrument_id)?;
        convert::quantity(self.filled_qty, precision)
    }
}

/// The instrument's real `size_precision`, or an error on a cache miss.
///
/// A guessed default (this used to fall back to a bare `8`) can silently
/// misrepresent a report's quantity precision against the real instrument -
/// exactly the class of hostile/unrepresentable-data problem `convert.rs`
/// otherwise takes care to surface rather than paper over. Both call sites
/// already thread the `anyhow::Result` through a `filter_map` that warns and
/// drops the one report on error, so surfacing the miss here costs nothing.
fn report_size_precision(
    instruments: &Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    instrument_id: InstrumentId,
) -> anyhow::Result<u8> {
    instrument_def(instruments, &symbol_from_instrument(instrument_id))
        .map(|def| def.size_precision)
        .with_context(|| format!("no instrument def cached for {instrument_id}"))
}

#[derive(Debug, Clone)]
struct FillRecord {
    client_order_id: ClientOrderId,
    instrument_id: InstrumentId,
    venue_order_id: VenueOrderId,
    trade_id: TradeId,
    order_side: OrderSide,
    last_qty: nautilus_model::types::Quantity,
    last_px: nautilus_model::types::Price,
    commission: Decimal,
    quote_currency: Currency,
    ts_event: UnixNanos,
}

#[derive(Debug, Clone)]
struct PositionRecord {
    symbol: Symbol,
    instrument_id: InstrumentId,
    quantity: Decimal,
    avg_px: Decimal,
    ts_last: UnixNanos,
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
        ServerMessage::AccountState(state) => handle_account_state(state, ctx),
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
        ServerMessage::Trade(_) | ServerMessage::Quote(_) => {}
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
    let Some((record, is_duplicate)) = with_order_record(&ctx.state, client_order_id, |record| {
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

    if !is_duplicate {
        let mut state = lock_recover(&ctx.state, "execution state");
        state.fills.push(FillRecord {
            client_order_id,
            instrument_id: record.instrument_id,
            venue_order_id,
            trade_id,
            order_side: record.order_side,
            last_qty,
            last_px,
            commission: fill.commission,
            quote_currency,
            ts_event: UnixNanos::from(fill.ts_event),
        });
        state.prune();
    }

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
    );
    ctx.emitter.send_order_event(OrderEventAny::Filled(event));
}

fn handle_account_state(state: mogwai_protocol::AccountState, ctx: &ExecContext) {
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
        // Snapshots must apply in venue order, not arrival order: the retain
        // below treats each snapshot as the newest truth, so an OLDER snapshot
        // delivered late by reorder/duplicate havoc would resurrect a closed
        // position or erase an open one, persistently (nothing corrects it
        // until the next fill-driven snapshot, which may be never). Skip any
        // snapshot below the applied watermark - and skip forwarding it to
        // nautilus too, since nautilus applies account states in arrival order
        // with no staleness guard of its own. Equal-ts duplicates pass; they
        // re-apply idempotently. The check and the apply share one lock so
        // concurrent HTTP dispatch drains cannot interleave between them.
        if ts_event < mirror.account_ts_last {
            tracing::warn!(
                ts_event = ts_event.as_u64(),
                last_applied = mirror.account_ts_last.as_u64(),
                "dropping stale account snapshot: older than the last applied one"
            );
            return;
        }
        mirror.account_ts_last = ts_event;
        // The engine reports a COMPLETE set of its non-zero positions in every
        // snapshot, and signals a flat instrument by OMITTING it: a position
        // closed to zero is removed from the engine's map, never reported as a
        // zero-qty entry. So the snapshot is authoritative - any symbol the
        // mirror still holds that is absent here has gone flat at the venue and
        // must be dropped. Without this drop the insert-only mirror keeps the
        // stale non-zero PositionRecord from the entry after the close, and
        // generate_position_status_reports then hands broadarrow a phantom
        // venue net it can only adopt as an EXTERNAL position - desyncing
        // reconciliation (slices no longer sum to net) and halting the account.
        let present: std::collections::HashSet<Symbol> =
            state.positions.iter().map(|p| p.symbol.clone()).collect();
        mirror
            .positions
            .retain(|symbol, _| present.contains(symbol));
        for position in state.positions {
            let Some(def) = instrument_def(&ctx.instruments, &position.symbol) else {
                // A legitimate missing def (instrument cache not yet seeded)
                // silently drops a real position from the account snapshot,
                // leaving the mirror inconsistent. The DropNextAccountUpdate
                // divergence drops the whole snapshot upstream, not individual
                // positions here, so warning does not mask it.
                tracing::warn!(
                    symbol = %position.symbol,
                    "dropping account position: no instrument def (cache not seeded?)"
                );
                continue;
            };
            mirror.positions.insert(
                position.symbol.clone(),
                PositionRecord {
                    symbol: position.symbol,
                    instrument_id: convert::instrument_id(&def),
                    quantity: position.quantity,
                    avg_px: position.avg_px,
                    ts_last: ts_event,
                },
            );
        }
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

    use mogwai_protocol::{ClientHavoc, HavocLatency, Side, TradeTick, TransportProfile};
    use nautilus_common::{cache::Cache, clients::ExecutionClient, messages::ExecutionEvent};
    use nautilus_live::{ExecutionClientCore, ExecutionEventEmitter};
    use nautilus_model::{
        enums::TimeInForce,
        identifiers::{ClientId, StrategyId, TraderId},
    };

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

    fn seed_order(state: &Arc<Mutex<ExecState>>) {
        state.lock().expect("execution state mutex").orders.insert(
            ClientOrderId::from("O-1"),
            OrderRecord {
                strategy_id: StrategyId::from("S-001"),
                instrument_id: instrument_id(),
                order_side: OrderSide::Buy,
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::Gtc,
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
                trader_id: config.trader_id,
                account_id: config.account_id,
                account_type: config.account_type,
                sim: SimClock::identity(),
            },
            rx,
        )
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
            .fills
            .push(FillRecord {
                client_order_id: ClientOrderId::from("O-1"),
                instrument_id: instrument_id(),
                venue_order_id: VenueOrderId::from("V-1"),
                trade_id: TradeId::from("T-1"),
                order_side: OrderSide::Buy,
                last_qty: nautilus_model::types::Quantity::new(1.0, 8),
                last_px: nautilus_model::types::Price::new(100.0, 2),
                commission: Decimal::ZERO,
                quote_currency: Currency::from_str("USDT").expect("usdt"),
                ts_event: UnixNanos::from(1),
            });

        client.reset().expect("reset succeeds");

        let state = client.state.lock().expect("execution state mutex");
        assert!(state.orders.is_empty(), "orders cleared on reset");
        assert!(state.fills.is_empty(), "fills cleared on reset");
        assert!(state.positions.is_empty(), "positions cleared on reset");
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
    async fn reports_reconstruct_from_mirror() {
        let mut client = execution_client();
        client.instruments = instruments_map();
        seed_order(&client.state);
        let (ctx, _rx) = exec_context();
        let ctx = ExecContext {
            emitter: ctx.emitter,
            state: Arc::clone(&client.state),
            instruments: Arc::clone(&client.instruments),
            trader_id: ctx.trader_id,
            account_id: ctx.account_id,
            account_type: ctx.account_type,
            sim: ctx.sim,
        };

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
        assert_eq!(orders[0].order_status, OrderStatus::Filled);
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].trade_id, TradeId::from("T-1"));
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].position_side, PositionSideSpecified::Long);
    }

    #[tokio::test]
    async fn closed_position_drops_from_mirror_so_reconciliation_reports_none() {
        // Regression: the engine omits a flat instrument from its account
        // snapshot (it removes a position closed to zero rather than reporting
        // zero qty). The insert-only mirror used to keep the stale entry
        // PositionRecord, so generate_position_status_reports handed broadarrow
        // a phantom venue net it adopted as an EXTERNAL position -> attribution
        // desync -> halted account. The close snapshot must clear the mirror.
        let mut client = execution_client();
        client.instruments = instruments_map();
        seed_order(&client.state);
        let (ctx, _rx) = exec_context();
        let ctx = ExecContext {
            emitter: ctx.emitter,
            state: Arc::clone(&client.state),
            instruments: Arc::clone(&client.instruments),
            trader_id: ctx.trader_id,
            account_id: ctx.account_id,
            account_type: ctx.account_type,
            sim: ctx.sim,
        };

        // Entry snapshot carries the open long.
        handle_exec_message(
            ServerMessage::AccountState(mogwai_protocol::AccountState {
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

        // Close snapshot: the engine has dropped the now-flat position, so the
        // snapshot lists none. The mirror must follow and drop BTCUSDT too.
        handle_exec_message(
            ServerMessage::AccountState(mogwai_protocol::AccountState {
                balances: Vec::new(),
                positions: Vec::new(),
                ts_event: 13,
            }),
            &ctx,
        );

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
            "a flattened position must not survive as a phantom reconciliation report"
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
        let state = ctx.state.lock().expect("execution state mutex");
        assert_eq!(state.fills.len(), 1, "the fill record must still be kept");
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
        assert!(
            state.positions.is_empty(),
            "a stale snapshot must not resurrect a closed position"
        );
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
        let state = ctx.state.lock().expect("execution state mutex");
        assert!(state.fills.is_empty(), "no fill record for a dropped fill");
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
        // generators: open orders, their fills, current positions.
        let mut client = execution_client();
        client.instruments = instruments_map();
        seed_order(&client.state);
        let (ctx, _rx) = exec_context();
        let ctx = ExecContext {
            emitter: ctx.emitter,
            state: Arc::clone(&client.state),
            instruments: Arc::clone(&client.instruments),
            trader_id: ctx.trader_id,
            account_id: ctx.account_id,
            account_type: ctx.account_type,
            sim: ctx.sim,
        };

        handle_exec_message(
            ServerMessage::OrderAccepted {
                client_order_id: "O-1".into(),
                venue_order_id: "V-1".into(),
                ts_event: 10,
            },
            &ctx,
        );
        // A partial fill keeps the order open, so it passes the open_only
        // filter the canonical mass-status shape applies to order reports.
        handle_exec_message(
            ServerMessage::OrderFilled(wire_fill("T-1", Decimal::new(1, 0), 11)),
            &ctx,
        );
        handle_exec_message(
            ServerMessage::AccountState(mogwai_protocol::AccountState {
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
            time_in_force: TimeInForce::Gtc,
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
        {
            let mut state = client.state.lock().expect("state");
            state.orders.insert(
                ClientOrderId::from("O-OPEN"),
                order_at(OrderStatus::Accepted, UnixNanos::from(1)),
            );
            state.orders.insert(
                ClientOrderId::from("O-CLOSED"),
                order_at(OrderStatus::Canceled, UnixNanos::from(1)),
            );
        }

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

    // AE10 (positions): every mirrored position is a current open venue position
    // (flat ones are dropped on snapshot apply), so a lookback-bounded start must
    // not hide a long-quiet resting position.
    #[tokio::test]
    async fn position_report_keeps_long_quiet_open_position() {
        let mut client = execution_client();
        client.instruments = instruments_map();
        client.state.lock().expect("state").positions.insert(
            "BTCUSDT".to_string(),
            PositionRecord {
                symbol: "BTCUSDT".to_string(),
                instrument_id: instrument_id(),
                quantity: Decimal::new(1, 0),
                avg_px: Decimal::new(10_000, 2),
                ts_last: UnixNanos::from(1),
            },
        );

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

    // AE6: ExecState.prune bounds the append-only fills Vec and terminal-order
    // records past their caps (oldest-first) while never dropping an open order.
    #[test]
    fn exec_state_prune_bounds_fills_and_terminal_orders_keeping_open() {
        let mut state = ExecState::default();
        for i in 0..(MAX_FILLS as u64 + 3) {
            state.fills.push(FillRecord {
                client_order_id: ClientOrderId::from("O-1"),
                instrument_id: instrument_id(),
                venue_order_id: VenueOrderId::from("V-1"),
                trade_id: TradeId::from("T-1"),
                order_side: OrderSide::Buy,
                last_qty: nautilus_model::types::Quantity::new(1.0, 8),
                last_px: nautilus_model::types::Price::new(100.0, 2),
                commission: Decimal::ZERO,
                quote_currency: Currency::from_str("USDT").expect("usdt"),
                ts_event: UnixNanos::from(i),
            });
        }
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

        assert_eq!(state.fills.len(), MAX_FILLS, "fills capped at MAX_FILLS");
        assert_eq!(
            state.fills.first().map(|fill| fill.ts_event),
            Some(UnixNanos::from(3)),
            "the oldest 3 fills were dropped"
        );
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
