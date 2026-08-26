// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! `MogwaiExecutionClient`: the `ExecutionClient` half of the adapter. Owns
//! the order/fill/position mirror (`ExecState`), the websocket order
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
    Command, FillSnapshot, HavocSpec, InstrumentDef, OrderStatusInfo, OrderStatusSnapshot,
    SimClock, Symbol, VenueMessage,
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
        LiquiditySide, OmsType, OrderSide, OrderStatus, OrderType, PositionSideSpecified,
        TriggerType,
    },
    events::{
        AccountState as NautilusAccountState, OrderAccepted, OrderCancelRejected, OrderCanceled,
        OrderEventAny, OrderExpired, OrderFilled, OrderModifyRejected, OrderRejected,
        OrderSubmitted, OrderTriggered, OrderUpdated,
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
        HavocDelivery, HavocFilter, abort_tasks, conn_havoc, enqueue_havoc,
        fetch_clock_or_identity, flush_havoc_into_pump, inbound_havoc, instrument_def, join_url,
        lock_recover, now_unix_nanos, request_timeout_secs, run_identity_check,
        schedule_reorder_flush, seed_instruments, spawn_latency_pump, symbol_from_instrument,
        track_task, wait_connected,
    },
    convert,
    lifecycle::{HttpQuota, WsConnectionConfig, run_ws_connection},
};

const ACCOUNT_REGISTRATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const ACCOUNT_REGISTRATION_POLL: std::time::Duration = std::time::Duration::from_millis(10);

/// Distinguishes a 404 (older venue without GET /account, the only
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

/// Pull the configured account's ledger from `GET /account`.
///
/// The account is always named, for the reason `ws_url` names it on the socket:
/// the venue resolves accounts totally, so a pull naming none is answered from
/// the run's default ledger whatever the host's config said, and nothing in the
/// answer says which ledger it describes. Naming it makes the configured id the
/// ledger this pull actually reads. The key must be spelled exactly `account`;
/// the venue's query carrier denies unknown fields, so a misspelling is a 400
/// rather than a quietly defaulted snapshot.
///
/// The id goes into the query string verbatim, with no percent encoding. A
/// `mogwai_protocol::AccountId` is ASCII alphanumerics plus dot, underscore,
/// colon and dash, and a nautilus `AccountId` is an `ISSUER-NUMBER` subset of
/// that. Every one of those characters is legal in a query value per RFC 3986,
/// colon included, so encoding would only obscure the id in a venue log.
async fn fetch_account(
    http: &HttpClient,
    quota: &HttpQuota,
    base: &str,
    account_id: AccountId,
) -> Result<mogwai_protocol::AccountState, FetchAccountError> {
    quota.wait().await;
    let url = format!(
        "{path}?account={account}",
        path = join_url(base, "account"),
        account = account_id.as_ref()
    );
    let response = http
        .get(
            url,
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
/// This was once a fatal equality check, and killing it is the point. Under an
/// earlier slot model a client named a slot and the venue honoured it, so a
/// snapshot bearing another id meant the venue had routed someone else's
/// account here and adopting it would have corrupted this one. The check was
/// removed when the venue collapsed to a single ledger; the venue has since
/// become per-account again (one ledger per account id, `GET /account` naming
/// whose with `?account=`), and [`fetch_account`] names the configured account
/// on every pull, so the venue answers for exactly the ledger this client is
/// configured to trade. The label the answer carries is therefore cosmetic:
/// which ledger was read is settled by the query, not by the id printed in the
/// snapshot, and the configured id stays authoritative for everything this
/// adapter emits.
///
/// It also could not be satisfied. The venue reported a bare `MOGWAI` for one
/// release - legal as a `mogwai_protocol::AccountId`, and unconstructable as a
/// nautilus one, which parses `ISSUER-NUMBER` - so no configured value could
/// equal it and every run died on connect. That specific id is fixed venue-side,
/// but a check that can deadlock a run over a label is worth removing on its own
/// terms rather than repairing.
///
/// Differing is still worth saying once, at connect: it means the venue config
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
/// Posts every arm in `spec.venue` to the venue's control plane, once, at
/// connect.
///
/// The account scope is per arm rather than blanket, because water-side and
/// terminal arms do not belong to a ledger. `arm_divergence` passes the
/// request's account through to `Run::arm` for every account-side arm.
/// `DelayAcks`, `CommandLatency`, `GoDark` and `StallData` blur one account's
/// view, so on a shared venue an unscoped one blacks out the whole batch rather
/// than this strategy. Engine arms and `FeeSurcharge` act on that account's
/// ledger, and `FaultTape`
/// refuses an account outright with a `400`, since there is no venue left to
/// scope. `CancelOpenOrderSilently` is the one scoped arm this carrier cannot
/// deliver at all; `validate_havoc` refuses it at config time, so nothing here
/// has to decide what to do with it.
async fn ship_venue_havoc(
    http: &HttpClient,
    http_base: &str,
    spec: &HavocSpec,
    account_id: AccountId,
    timeout_secs: u64,
) -> anyhow::Result<()> {
    let url = join_url(http_base, "control/divergence");
    for divergence in &spec.venue {
        let serde_json::Value::Object(mut encoded) =
            serde_json::to_value(divergence).context("encode divergence")?
        else {
            unreachable!("Divergence always serializes as an object")
        };
        let kind = encoded
            .remove("type")
            .expect("Divergence serialization carries its tag");
        let mut request = serde_json::json!({
            "kind": kind,
            "args": encoded,
        });
        // Named rather than defaulted, and the set is the one this function's
        // doc derives from `arm_divergence`: every account-side arm carries the
        // named account. A `_` arm here would
        // silently start scoping the next variant somebody adds, which is how a
        // field gets sent to a reader that ignores it.
        if matches!(
            divergence,
            mogwai_protocol::control::Divergence::DelayAcks { .. }
                | mogwai_protocol::control::Divergence::CommandLatency { .. }
                | mogwai_protocol::control::Divergence::GoDark { .. }
                | mogwai_protocol::control::Divergence::StallData { .. }
                | mogwai_protocol::control::Divergence::PartialFillNext { .. }
                | mogwai_protocol::control::Divergence::RejectNextSubmit { .. }
                | mogwai_protocol::control::Divergence::RejectNextCancel { .. }
                | mogwai_protocol::control::Divergence::DuplicateNextFill
                | mogwai_protocol::control::Divergence::DropNextAccountUpdate
                | mogwai_protocol::control::Divergence::FeeSurcharge { .. }
        ) {
            request["account"] = serde_json::Value::String(account_id.to_string());
        }
        let body = serde_json::to_vec(&request).context("encode divergence request")?;
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
    connected_notify: Arc<tokio::sync::Notify>,
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
    /// Handles for this client's spawned tasks: the WS reader, the havoc
    /// latency pump, and every `query_order` probe. Shared behind an
    /// `Arc<Mutex<..>>` so the `&self` handlers can record a task alongside the
    /// `&mut self` connect path; `stop()` aborts the lot so a task that
    /// outlived the client cannot emit exec events (its emitter still holds a
    /// live sender clone) after the client stopped (AE19).
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
            connected_notify: Arc::new(tokio::sync::Notify::new()),
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
    /// replacing the shared cell outright, not by storing `false` into it.
    ///
    /// `abort_tasks` is not synchronous: cancellation is delivered at the
    /// aborted task's next await point, so a reader caught between
    /// `connect_async(..).await` returning and its first select can still run
    /// `connected.store(true)` after the caller has stored `false`. That
    /// leaves a stopped client reporting itself connected, and a subsequent
    /// `wait_connected` returning success for a socket that never opened.
    /// Swapping the `Arc` means the retired reader writes to a cell nobody
    /// reads: the drop timing of the old generation stops being a
    /// correctness property.
    fn retire_connected_flag(&mut self) {
        self.connected = Arc::new(AtomicBool::new(false));
        self.connected_notify = Arc::new(tokio::sync::Notify::new());
    }

    /// The status this client's reconciliation mirror currently holds for one
    /// order, or `None` if the mirror does not know it.
    ///
    /// Read-only, and deliberately the mirror rather than venue truth: the
    /// mirror is the adapter's own belief about an order, and a class of defect
    /// exists that is invisible from every other surface - an amend ack that
    /// recomputes status from fill progress alone walks a `Triggered` conditional
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

    /// One nautilus `OrderInitialized` as this venue's wire submit, linkage and
    /// all. Shared by the single-order and order-list paths so the two cannot
    /// disagree about what a leg means.
    ///
    /// A host-stated price on a `Market` order is never dropped here, and never
    /// left for the socket to discover either. `MarketOrder`'s own constructors
    /// cannot produce one - `MarketOrder::from(OrderInitialized)` asserts the
    /// price is absent - but that is not the event this method receives:
    /// `SubmitOrder::new` takes an arbitrary `OrderInitialized`, every field of
    /// which is `pub`, so `order_type = Market` beside `price = Some(..)` is
    /// reachable through nautilus's public API with nothing to stop it. Dropping
    /// the price would hide that; forwarding it alone would make the refusal
    /// depend on a live socket and arrive as a venue rejection for an event the
    /// adapter could already see was malformed.
    ///
    /// So the built frame is run through `validate_submit_order` at
    /// `SubmitPhase::PreStamp` before it leaves - the venue's own decode-boundary
    /// verdict (`mogwai_venue::http::boundary_error` calls the same function at
    /// the same phase), not a second copy of its rule table that could drift from
    /// it. A malformed init therefore fails conversion, which by the AE8 ordering
    /// above returns before any event is emitted or any mirror record exists, and
    /// nautilus denies the order with the venue's own reason on it.
    ///
    /// The `TrailingStopLimit` exception below is different in kind: nautilus has
    /// a stated limit field that mogwai deliberately represents as an offset, so
    /// dropping it is a representation change rather than a discarded refusal.
    fn wire_submit(
        &self,
        client_order_id: &ClientOrderId,
        instrument_id: InstrumentId,
        position_id: Option<PositionId>,
        init: &nautilus_model::events::OrderInitialized,
    ) -> anyhow::Result<mogwai_protocol::SubmitOrder> {
        convert::wire_trigger_type(init.trigger_type)?;
        if init
            .trigger_instrument_id
            .is_some_and(|id| id != instrument_id)
        {
            anyhow::bail!(
                "cross-instrument triggers are unsupported: MOGWAI triggers from the order instrument's tape"
            );
        }
        let wire = mogwai_protocol::SubmitOrder {
            client_order_id: client_order_id.to_string(),
            symbol: symbol_from_instrument(instrument_id),
            position_id: position_id.map(|id| id.to_string()),
            side: convert::wire_side(init.order_side)?,
            order_type: convert::wire_order_type(init.order_type)?,
            quantity: init.quantity.as_decimal(),
            price: match init.order_type {
                OrderType::TrailingStopLimit => None,
                _ => init.price.map(|p| p.as_decimal()),
            },
            trigger_price: init.trigger_price.map(|p| p.as_decimal()),
            // Nautilus states a trailing offset with a type beside it - price,
            // ticks, basis points. Only the price form maps: the venue's trail
            // is an absolute distance, and converting the others needs a
            // reference price the two ends would have to agree on separately.
            trail_offset: convert::wire_trail_offset(
                init.trailing_offset,
                init.trailing_offset_type,
            )?,
            // The limit half of a trailing stop limit, under the same
            // offset-type restriction and for the same reason. The venue
            // derives the limit price from it, so a nautilus-stated `price` on
            // this type is dropped rather than forwarded - the wire refuses one
            // there, and forwarding it would trade a working order for a
            // rejection over a field the first ratchet would have overwritten.
            limit_offset: match init.order_type {
                OrderType::TrailingStopLimit => {
                    convert::wire_trail_offset(init.limit_offset, init.trailing_offset_type)?
                }
                _ => None,
            },
            reduce_only: init.reduce_only,
            post_only: init.post_only,
            time_in_force: convert::wire_time_in_force(init.time_in_force)?,
            expire_time: init.expire_time.map(|ts| ts.as_u64()),
            link: convert::wire_order_link(init)?,
        };
        if let Err(reason) =
            mogwai_protocol::validate_submit_order(&wire, mogwai_protocol::SubmitPhase::PreStamp)
        {
            anyhow::bail!(
                "this OrderInitialized is malformed for MOGWAI: {reason}. Fix the event the \
                 strategy submitted; the venue refuses this frame at its decode boundary"
            );
        }
        Ok(wire)
    }

    /// Mirror the order locally and tell nautilus it is on its way.
    ///
    /// Emitted only once conversion has succeeded and the mirror record exists.
    /// The dispatch that follows may still fail at transport (the WS command
    /// channel is gone), in which case `dispatch_order` synthesizes the
    /// matching `OrderRejected` - a valid Submitted -> Rejected transition - so
    /// the order still reaches a terminal state.
    fn announce_submitted(
        &self,
        client_order_id: &ClientOrderId,
        strategy_id: nautilus_model::identifiers::StrategyId,
        instrument_id: InstrumentId,
        init: &nautilus_model::events::OrderInitialized,
        ts_command: UnixNanos,
    ) -> anyhow::Result<()> {
        let submitted = self.build_submitted(client_order_id)?;
        self.commit_submitted(
            submitted,
            client_order_id,
            strategy_id,
            instrument_id,
            init,
            ts_command,
        );
        Ok(())
    }

    /// The fallible half of an announcement: resolve the cached order and build
    /// its `OrderSubmitted`. Nothing here is observable, which is what lets a
    /// multi-leg submission resolve every leg before any leg is announced.
    fn build_submitted(&self, client_order_id: &ClientOrderId) -> anyhow::Result<OrderSubmitted> {
        let order = self.core.get_order(client_order_id)?;
        let ts_init = now_unix_nanos(self.sim);
        Ok(OrderSubmitted::new(
            self.core.trader_id,
            order.strategy_id(),
            order.instrument_id(),
            order.client_order_id(),
            self.core.account_id,
            UUID4::new(),
            ts_init,
            ts_init,
        ))
    }

    /// The infallible half: insert the mirror record and emit the event. It
    /// takes no `Result` on purpose - once the first leg of a list has been
    /// announced there is no honest way to fail the second, so the mutex is
    /// taken through `lock_recover` (poison-recovering, as everywhere else in
    /// this client) rather than through a `?`.
    ///
    /// This applies to both callers, and the single-order path inherits a
    /// behavior change: `announce_submitted` used to return `Err("execution state mutex
    /// poisoned")`, and through `submit_order` that failure mode is now gone.
    /// Deliberate, and not merely collateral to the list fix. `lock_recover` is
    /// the idiom everywhere else in this client and in the data client, for the
    /// reason it was adopted: this mutex guards a mirror of venue state, a
    /// poisoning means some other thread panicked mid-update, and refusing
    /// every subsequent submission forever is a worse answer than recovering
    /// the guard and continuing against a possibly-stale mirror that the next
    /// venue message corrects. A caller that saw the old error had no recovery
    /// for it either.
    fn commit_submitted(
        &self,
        submitted: OrderSubmitted,
        client_order_id: &ClientOrderId,
        strategy_id: nautilus_model::identifiers::StrategyId,
        instrument_id: InstrumentId,
        init: &nautilus_model::events::OrderInitialized,
        ts_command: UnixNanos,
    ) {
        {
            let mut state = lock_recover(&self.state, "exec state");
            state.orders.insert(
                *client_order_id,
                OrderRecord {
                    strategy_id,
                    instrument_id,
                    order_side: init.order_side,
                    order_type: init.order_type,
                    status: OrderStatus::Submitted,
                    venue_order_id: None,
                    ts_last: ts_command,
                    seen_trades: std::collections::HashSet::new(),
                },
            );
            state.prune();
        }
        self.emitter
            .send_order_event(OrderEventAny::Submitted(submitted));
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
            // logs an Err from cancel_order/modify_order (no event), so without
            // this a cancel/modify would sit forever in PendingCancel/PendingUpdate
            // (and a submit in Submitted) with no reject to restore it. Synthesize
            // the matching reject here and report success: the reject event is the
            // signal, not the return value, which is the same contract the
            // reader's undelivered-command path answers under (AE9).
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
        let target = client_order_id.clone();
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
        let mut snapshot = self
            .await_reply(reply_rx, &request_id, |pending, id| {
                pending.orders.remove(id);
            })
            .await?;
        // The frontier rule applied to an answer: a targeted query resolves one
        // order's in-flight state, so a row for any other order is discarded
        // here rather than handed on as that order's truth. Central on purpose -
        // `query_order` reads `orders.first()` and
        // `generate_order_status_report` filters only by venue order id and
        // instrument, so neither singular path re-checks the identity it asked
        // for. The venue does filter correctly today; this is the adapter
        // declining to rest a probe's correctness on the other end's filter.
        if let Some(target) = target {
            snapshot
                .orders
                .retain(|info| info.client_order_id == target);
        }
        Ok(snapshot)
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

    /// Register the waiter before sending the command, so a reply racing back
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
    /// request timeout - the same ceiling the remaining HTTP fetches get from
    /// their client - and clean the pending slot up on timeout so a reply that
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
        info: Option<nautilus_core::Params>,
    ) -> anyhow::Result<()> {
        self.emitter.send_account_state(
            NautilusAccountState::new(
                self.core.account_id,
                self.config.account_type,
                balances,
                margins,
                reported,
                UUID4::new(),
                ts_event,
                now_unix_nanos(self.sim),
                None,
            )
            // The venue-specific `info` bag is the caller's, not ours: pass it
            // through rather than dropping it, so a host that attaches account
            // metadata sees it on the emitted event.
            .with_info(info),
        );
        Ok(())
    }

    fn start(&mut self) -> anyhow::Result<()> {
        if self.core.is_started() {
            return Ok(());
        }

        // A `None` here is not fatal on its own - `start()` is also called in
        // contexts with no runner - but it is the only chance this thread has
        // to see the runner's thread-local, so say so rather than swallowing
        // it. `connect()` refuses later if nothing ever installs a sender.
        if let Some(sender) = try_get_exec_event_sender() {
            self.emitter.set_sender(sender);
        } else if !self.emitter.is_initialized() {
            tracing::warn!(
                "no execution event sender on this thread at start(): connect() will refuse \
                 until one is installed"
            );
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
        // The group ring is transport state, never mirror state, so it dies with
        // the transport. `reset()` cleared it only incidentally, by replacing
        // the whole `ExecState`; a plain stop-and-connect left it standing, and
        // a list id repeating across generations - nautilus ids are per-strategy
        // and a restarted strategy reuses them - would then attribute the new
        // generation's group refusal to the old generation's legs, rejecting
        // orders that were never refused. The mirror's orders and account
        // watermark deliberately survive a stop; these do not, because their
        // whole purpose is to answer a refusal from the socket that just died.
        lock_recover(&self.state, "exec state").groups.clear();
        self.core.set_stopped();
        self.core.set_disconnected();
        Ok(())
    }

    fn reset(&mut self) -> anyhow::Result<()> {
        // Mirror MogwaiDataClient::reset: stop first (abort tasks, drop the WS
        // command channel), then clear the reconciliation mirror. Without this
        // the default no-op `reset` leaves ExecState.orders and the account
        // staleness watermark populated across a stop/start, so a prior
        // passenger's orders leak into the next passenger's status/fill reports
        // and its watermark makes the new passenger's first account snapshot
        // look stale.
        self.stop()?;
        let mut state = lock_recover(&self.state, "exec state");
        *state = ExecState::default();
        Ok(())
    }

    async fn connect(&mut self) -> anyhow::Result<()> {
        // A client owns exactly one transport generation. This cleanup is
        // unconditional and independent of the component started flag: a host
        // may reconnect after stop. It may never connect without starting - see
        // the emitter guard below, which refuses that ordering outright. The
        // host-facing statement of this contract is `docs/adapter-lifecycle.md`.
        self.ws_cmd = None;
        abort_tasks(&self.task_handles);
        self.retire_connected_flag();
        {
            let mut pending = lock_recover(&self.pending, "pending queries");
            pending.orders.clear();
            pending.fills.clear();
        }
        // Refuse a deaf connection outright (AE20). Nautilus's `ExecutionEventEmitter`
        // derives Clone and owns `sender: Option<..>` by value, so every
        // `exec_context()` clone - the WS pump's above all - freezes whatever
        // sender state exists at the instant it is taken. The sender is
        // installed exactly once, by `start()`, and `send_order_event` on an
        // emitter without one only writes `log::warn!("Cannot send order
        // event: sender not initialized")`: no error, no return value. So a
        // host that connects without starting gets a client that reads as
        // connected and is not - accepts, fills, cancels and rejects all
        // vanish for the life of the connection, silently.
        //
        // The failure is worse than a dead stream because it is asymmetric:
        // `submit_order` emits its own `Submitted` off the live emitter field
        // rather than through a frozen clone, so nautilus keeps seeing every
        // order go Submitted and nothing after it, forever. That reads as a
        // wedged venue rather than as a broken client.
        //
        // No shipped host hits this: nautilus's own kernel starts clients
        // before connecting them, on one current-thread runtime. So this is a
        // host-ordering contract we state and enforce, not a repair of an
        // ordering we expect.
        //
        // The sender cannot simply be resolved here for everyone:
        // `try_get_exec_event_sender` reads a thread-local set on the runner's
        // thread, and this async fn may already be polled on another. Try it
        // anyway - it costs nothing and succeeds whenever connect really is on
        // that thread - and refuse loudly when it does not, rather than
        // proceeding into a run whose every venue event is dropped.
        if !self.emitter.is_initialized()
            && let Some(sender) = try_get_exec_event_sender()
        {
            self.emitter.set_sender(sender);
        }
        ensure!(
            self.emitter.is_initialized(),
            "execution event sender not initialized: call start() on the runner's thread before \
             connect(), or every order event this connection receives would be dropped silently"
        );
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
            ship_venue_havoc(
                &self.http,
                &http_base_url,
                havoc,
                self.config.account_id,
                timeout_secs,
            )
            .await?;
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
        // Scope: this runs on connect (and so on reset/reconnect driven by
        // nautilus, which calls connect again), never on the transport's own
        // internal reconnect - the lifecycle reattach replays WS commands only.
        // A pushed account update lost to a blackout spanning an internal
        // reconnect therefore stays lost until the next snapshot any order
        // transition pushes,
        // deliberately; see the reattach comment in lifecycle.rs for why
        // auto-healing it would undo an armed divergence.
        //
        // Failure policy: a 404 means a venue predating GET /account; warn and
        // fall back to the legacy reactive path (the account seeds off the first
        // fill, as before this fix). Any other failure against a venue that does
        // publish the route is fatal - warn-and-continue there would silently
        // recreate the exact first-fill cache-miss this fix exists to eliminate.
        match fetch_account(
            &self.http,
            &self.http_quota,
            &http_base_url,
            self.config.account_id,
        )
        .await
        {
            Ok(state) => {
                note_account_label(&state, self.config.account_id);
                handle_exec_message(VenueMessage::AccountState(state), &self.exec_context());
                self.await_account_registered().await?;
            }
            Err(FetchAccountError::NotFound) => {
                tracing::warn!("venue predates GET /account; account will seed on first fill");
            }
            Err(err) => {
                return Err(anyhow::Error::new(err).context("initial account snapshot"));
            }
        }
        let inbound_havoc = inbound_havoc(&self.config.havoc);

        // `ws_url` already carries the `/ws` path.
        let ws_url = self.config.ws_url();
        let (cmd_tx, cmd_rx) = unbounded_channel::<ExecWsCommand>();
        self.ws_cmd = Some(cmd_tx);
        let undelivered_ctx = self.exec_context();

        let connected = Arc::clone(&self.connected);
        let havoc_filter = Arc::new(tokio::sync::Mutex::new(HavocFilter::from_inbound(
            &inbound_havoc,
        )));
        // Exec drain pipelines the per-message havoc latency through a pump
        // rather than sleeping inline in the reader loop, which capped throughput
        // and head-of-line-blocked pings/commands (AD4 - see the data client and
        // spawn_latency_pump). The pump owns a clone of the exec context and
        // applies each event to the mirror off-loop. handle_exec_message is
        // already called from off this loop - by the transport rejects
        // `dispatch_order` synthesizes on whatever thread nautilus dispatched
        // the command from, and by the reader's undelivered-command callback -
        // so moving the WS drain's calls onto the pump task adds no new sharing.
        let (deliver_tx, deliver_rx) = unbounded_channel::<HavocDelivery>();
        // The delivery barrier. The venue attaches this socket to the live tape
        // at upgrade, so frames can arrive before the post-bind reseed below has
        // read `/instruments` - and a frame naming an instrument the cache has
        // not got is dropped, not retried. The reader still enqueues; the pump
        // holds its first delivery until the reseed says go, so nothing reaches
        // a handler before the def it needs is resident. Held, not dropped: the
        // frames are real tape.
        let (delivery_ready, pump_ready) = tokio::sync::watch::channel(false);
        let pump_ctx = self.exec_context();
        let pump_handle = spawn_latency_pump(deliver_rx, move |msg| {
            let ctx = pump_ctx.clone();
            let mut pump_ready = pump_ready.clone();
            async move {
                // An `Err` means connect dropped the sender - the barrier will
                // never open, so deliver rather than wedge the pump.
                drop(pump_ready.wait_for(|open| *open).await);
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
            "exec",
        );
        let dial_timeout = std::time::Duration::from_secs(self.config.dial_timeout_secs);
        let connected_notify = Arc::clone(&self.connected_notify);
        let reader_handle = tokio::spawn(async move {
            run_ws_connection(
                WsConnectionConfig {
                    ws_url: task_ws_url,
                    conn,
                    seed: inbound_havoc.seed,
                    connected,
                    connected_notify,
                    sim,
                    label: "exec",
                    identity,
                    dial_timeout,
                },
                Some(cmd_rx),
                exec_command_to_client_message,
                Vec::new,
                move |venue_msg| {
                    let handler_filter = Arc::clone(&handler_filter);
                    let handler_deliver = handler_deliver.clone();
                    async move {
                        let mut filter = handler_filter.lock().await;
                        enqueue_havoc(&mut filter, venue_msg, sim, &handler_deliver);
                        if let Some(token) = filter.held_token() {
                            schedule_reorder_flush(
                                Arc::clone(&handler_filter),
                                token,
                                sim,
                                handler_deliver.clone(),
                            );
                        }
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
                // A command the socket swallowed is a transport failure, and it
                // is reported through exactly the path that already handles the
                // channel-closed case: the same synthesized reject, from the
                // same context. Before this existed, a submit queued in the
                // millisecond before a socket drop reached nautilus as
                // `Submitted` and then never received another event - the wedge
                // AE9 closed for the channel-closed case, still open for the
                // writer-aborted one.
                //
                // The context is cloned here rather than resolved per call:
                // this closure lives in the reader task and `&self` is not
                // available there. Every field is an `Arc` or a `Copy` handle
                // onto the same shared state `dispatch_order` uses, so a reject
                // synthesized from it lands in the same mirror and on the same
                // emitter.
                move |cmd: ExecWsCommand| report_undelivered_command(&cmd, &undelivered_ctx),
            )
            .await;
        });

        track_task(&self.task_handles, reader_handle);
        // See MogwaiDataClient::connect: a timed-out connect must abort the
        // just-spawned reader and clear the stale handle/ws_cmd so a retry does
        // not orphan the first task racing on the shared `connected` flag.
        if let Err(err) = wait_connected(
            &self.connected,
            &self.connected_notify,
            &ws_url,
            dial_timeout,
        )
        .await
        {
            abort_tasks(&self.task_handles);
            self.retire_connected_flag();
            self.ws_cmd = None;
            return Err(err);
        }
        // The post-bind reseed. Binding is what registers an unconfigured symbol
        // venue-side, so the pre-dial seed above cannot have carried its def;
        // only a read after the socket is up can. The pre-dial seed and the
        // account snapshot keep their order deliberately - the snapshot resolves
        // against configured defs and must stay ahead of the socket.
        if let Err(err) = seed_instruments(
            &self.http,
            &self.http_quota,
            &http_base_url,
            &self.instruments,
        )
        .await
        {
            // Same teardown as a timed-out connect: leave nothing running that
            // a retry would race, and never leave the barrier shut on a live
            // pump.
            abort_tasks(&self.task_handles);
            self.retire_connected_flag();
            self.ws_cmd = None;
            return Err(err);
        }
        if delivery_ready.send(true).is_err() {
            // No receiver: the pump task is already gone, so there is nothing
            // held behind the barrier to release.
            tracing::debug!("released the delivery barrier with no pump listening");
        }
        self.core.set_connected();
        Ok(())
    }

    async fn disconnect(&mut self) -> anyhow::Result<()> {
        self.stop()
    }

    fn submit_order(&self, cmd: SubmitOrder) -> anyhow::Result<()> {
        // Build the wire order first (AE8). An unsupported side/type/TIF errors
        // out of convert::wire_* here; emitting OrderSubmitted before this - as
        // the code used to - queued a Submitted event that nautilus then had to
        // apply to an order it had already denied (Initialized -> Denied) on the
        // very same failed conversion, producing a stray event, a scary
        // invalid-transition log, and (on a later send failure) a permanently
        // Submitted mirror stray that fed the unbounded ExecState growth.
        // Converting first means a conversion failure returns before any event is
        // emitted or any mirror record exists.
        let wire = self.wire_submit(
            &cmd.client_order_id,
            cmd.instrument_id,
            cmd.position_id,
            &cmd.order_init,
        )?;
        self.announce_submitted(
            &cmd.client_order_id,
            cmd.strategy_id,
            cmd.instrument_id,
            &cmd.order_init,
            cmd.ts_init,
        )?;
        self.dispatch_order(&ExecWsCommand::Submit(wire))
    }

    /// An order list, submitted leg by leg in the list's own order.
    ///
    /// The legs go out as ordinary submits carrying their linkage, because that
    /// is what the venue models: a group id plus a rule each member holds, not a
    /// list object the venue would have to own. Order matters and is nautilus's,
    /// not ours - an `OrderList` puts the parent first, and the venue refuses a
    /// child whose parent it has not seen, so re-ordering here would break a
    /// bracket that nautilus assembled correctly.
    ///
    /// A leg that fails conversion aborts the whole list before anything is
    /// dispatched, which is the only honest failure mode: half a bracket is
    /// worse than none, and a strategy that gets a rejection for its entry can
    /// retry, while one whose stop silently never reached the venue cannot.
    fn submit_order_list(
        &self,
        cmd: nautilus_common::messages::execution::SubmitOrderList,
    ) -> anyhow::Result<()> {
        let mut wires = Vec::with_capacity(cmd.order_inits.len());
        for init in &cmd.order_inits {
            wires.push((
                init.client_order_id,
                self.wire_submit(
                    &init.client_order_id,
                    init.instrument_id,
                    cmd.position_id,
                    init,
                )?,
            ));
        }
        // Announce every leg first, then dispatch one frame. The announcement
        // is nautilus-side bookkeeping and has to precede the venue's answer;
        // the dispatch is a single `SubmitOrderGroup`, because per-leg submits
        // are what let leg one fill before leg two is admitted. That is the
        // hazard the group frame exists to close, and the venue now refuses the
        // per-leg route for linked orders outright.
        //
        // The announcement loop is pass-invariant by construction. Resolving
        // the cached order is fallible, and doing it inside the announcing loop
        // meant a failure on leg three left legs one and two already emitted as
        // `OrderSubmitted` and already mirrored, while the `?` returned before
        // `dispatch_order` - so no `SubmitGroup` frame went out, no reject was
        // synthesized, and the announced prefix sat `Submitted` forever in both
        // nautilus and the mirror. That is exactly the half-a-bracket outcome
        // the doc above claims this method prevents. Build every leg's event
        // first: the fallible pass is now entirely unobservable, and the pass
        // that emits cannot fail.
        let mut built = Vec::with_capacity(wires.len());
        for (client_order_id, wire) in wires {
            built.push((
                self.build_submitted(&client_order_id)?,
                client_order_id,
                wire,
            ));
        }
        let mut orders = Vec::with_capacity(built.len());
        let mut members = Vec::with_capacity(built.len());
        for ((submitted, client_order_id, wire), init) in built.into_iter().zip(&cmd.order_inits) {
            self.commit_submitted(
                submitted,
                &client_order_id,
                cmd.strategy_id,
                init.instrument_id,
                init,
                cmd.ts_init,
            );
            members.push(client_order_id);
            orders.push(wire);
        }
        // Remembered so an `AdmissionSubject::SubmitGroup` refusal - which names
        // the list rather than its members, because a group is refused whole -
        // can be turned back into one `OrderRejected` per leg. Without it a
        // refused bracket would leave every leg of it waiting on an answer the
        // venue has already given.
        if let Some(list_id) = group_id_of(
            orders
                .iter()
                .map(|order| order.link.as_ref().map(|link| link.order_list_id.as_str())),
        ) {
            lock_recover(&self.state, "exec state").remember_group(list_id, members);
        }
        self.dispatch_order(&ExecWsCommand::SubmitGroup(orders))
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

    /// Order status reports from venue truth, not the adapter-side mirror.
    ///
    /// The mirror is populated by the same lifecycle stream havoc corrupts,
    /// so a report built from it can only repeat the client's (possibly
    /// stale) belief - in the exact fault class reconciliation exists to
    /// catch (a venue-side cancel whose event was dropped), a mirror-based
    /// report confidently confirms the stale open order. Querying the venue
    /// over the wire makes this generator a second, independent witness: the
    /// reply content is always a truthful engine book read (honest-content
    /// contract on `Command::QueryOrders`), while havoc may still
    /// delay or drop its delivery - which surfaces here as a query timeout,
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
    /// in-flight order and forced the consumer's local `INFLIGHT_TIMEOUT`
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

    /// Fill reports from venue truth (see `generate_order_status_reports`):
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

    /// Position status reports from venue truth, completing the set alongside
    /// `generate_order_status_reports` and `generate_fill_reports`.
    ///
    /// These used to be rebuilt from the adapter-side account-snapshot mirror,
    /// which is populated by the same pushed `AccountState` stream havoc
    /// corrupts - and mogwai ships a divergence, `DropNextAccountUpdate`, whose
    /// entire purpose is swallowing one of those pushes. A mirror-built report
    /// therefore confidently confirms a stale position in precisely the fault
    /// class position reconciliation exists to catch, which is the same
    /// argument that moved the order and fill generators onto the venue-truth
    /// surface.
    ///
    /// `GET /account` is the truthful source and is deliberately not the pushed
    /// frame: it is a point-in-time pull that bypasses the `HavocFilter`, so an
    /// armed `DropNextAccountUpdate` cannot suppress it (see `connect`'s
    /// initial snapshot). It is also an ordinary HTTP pull that touches neither
    /// the WS command channel nor the reader, so it answers whatever the exec
    /// socket is doing - including a reconnect in progress.
    ///
    /// A failed pull propagates rather than falling back to any client-side
    /// belief: an error makes reconciliation fail loudly, whereas a silent
    /// fallback would reintroduce the stale confirmation this exists to remove.
    /// The one exception is a 404, which `connect` already treats as a venue
    /// predating the route and continues past - failing here would turn that
    /// documented legacy path into a hard failure of the whole mass status,
    /// taking the order and fill reports down with it.
    async fn generate_position_status_reports(
        &self,
        cmd: &GeneratePositionStatusReports,
    ) -> anyhow::Result<Vec<PositionStatusReport>> {
        let state = match fetch_account(
            &self.http,
            &self.http_quota,
            &self.config.http_base_url(),
            self.config.account_id,
        )
        .await
        {
            Ok(state) => state,
            Err(FetchAccountError::NotFound) => {
                tracing::warn!(
                    "venue predates GET /account; reporting no positions rather than failing \
                     the whole mass status - position reconciliation is blind against this venue"
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
                // Match the whole instrument id, venue included. The wire rows
                // carry only a symbol, so comparing symbols alone would let a
                // request scoped to BTCUSDT on some other venue match this
                // venue's BTCUSDT position - a filter the caller asked for and
                // did not get. Every row here is by construction a `MOGWAI` one.
                cmd.instrument_id.is_none_or(|id| {
                    id.venue == *MOGWAI_VENUE && symbol_from_instrument(id) == position.symbol
                })
            })
            .filter(|position| {
                // Every position the venue reports is a current open (nonzero)
                // one - the engine removes a symbol from its position map the
                // moment it goes flat - so a lookback-bounded `start` must not
                // hide a long-quiet resting position; reconciliation would
                // otherwise have to re-adopt it as external mid-run (AE10).
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
    /// adapter error)" and then reconciles nothing at all - a worker restarted while
    /// holding an open mogwai position would boot flat and only discover the
    /// venue net via the periodic position poll, mid-run, as a late external
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
                    .saturating_sub(mins.saturating_mul(60 * crate::clock::NANOS_PER_SEC)),
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
        // The lookback is asked unbounded and narrowed here rather than being
        // pushed into the generator, for two reasons. An order report carries
        // no `avg_px`, so a partially filled open order can only be paired from
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
    /// A whole linked group in one frame, which is the only way a bracket may
    /// reach this venue. Sending the legs as separate `Submit`s lets leg one
    /// fill before leg two is admitted - the `Ouo` shrink then adjusts a
    /// sibling that is not on the book, leg two arrives at full size, and the
    /// pair's aggregate fill is twice the bracket quantity. The venue refuses a
    /// linked bare `Submit` for exactly that reason, so this is not merely the
    /// preferred route, it is the route.
    SubmitGroup(Vec<mogwai_protocol::SubmitOrder>),
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

/// Takes the command by reference and clones what the wire message needs,
/// because the lifecycle keeps the original until it has observed the frame
/// reach the socket - that is what lets it report the command back if the
/// socket dies first. The clone is one order's worth of `String`s and
/// `Decimal`s, on the order-entry path rather than the tick path.
fn exec_command_to_client_message(cmd: &ExecWsCommand) -> Command {
    match cmd {
        ExecWsCommand::Submit(order) => Command::SubmitOrder(order.clone()),
        ExecWsCommand::SubmitGroup(orders) => Command::SubmitOrderGroup {
            orders: orders.clone(),
        },
        ExecWsCommand::Cancel { client_order_id } => Command::CancelOrder {
            client_order_id: client_order_id.clone(),
        },
        ExecWsCommand::Modify {
            client_order_id,
            price,
            quantity,
            trigger_price,
        } => Command::ModifyOrder {
            client_order_id: client_order_id.clone(),
            price: *price,
            quantity: *quantity,
            trigger_price: *trigger_price,
        },
        ExecWsCommand::QueryOrders {
            request_id,
            client_order_id,
            open_only,
        } => Command::QueryOrders {
            request_id: request_id.clone(),
            client_order_id: client_order_id.clone(),
            open_only: *open_only,
        },
        ExecWsCommand::QueryFills {
            request_id,
            client_order_id,
        } => Command::QueryFills {
            request_id: request_id.clone(),
            client_order_id: client_order_id.clone(),
        },
    }
}

/// Maps a transport-level failure (the command's frame never reached the
/// venue: the WS command channel was gone, or the writer was aborted before it
/// sent) onto the `VenueMessage` shape `handle_exec_message` already knows how
/// to turn into a nautilus event. Only valid for `Submit`/`Modify`: a failed
/// `Cancel` is not a full order rejection (the order is still live, or its fate
/// is simply unknown) and is handled by `emit_cancel_rejected` before the call
/// site ever reaches this function - see `synthesize_transport_reject`.
fn reject_for(cmd: &ExecWsCommand, err: &anyhow::Error, sim: SimClock) -> VenueMessage {
    let reason = err.to_string();
    let ts_event = now_unix_nanos(sim).as_u64();
    match cmd {
        ExecWsCommand::Submit(order) => VenueMessage::OrderRejected {
            client_order_id: order.client_order_id.clone(),
            reason,
            ts_event,
        },
        ExecWsCommand::Modify {
            client_order_id, ..
        } => VenueMessage::OrderModifyRejected {
            client_order_id: client_order_id.clone(),
            venue_order_id: None,
            reason,
            ts_event,
        },
        ExecWsCommand::Cancel { client_order_id } => unreachable!(
            "cancel transport failures are reported via emit_cancel_rejected, \
             not reject_for (client_order_id={client_order_id})"
        ),
        ExecWsCommand::SubmitGroup(_) => unreachable!(
            "a group's transport failure rejects every leg and is handled in \
             synthesize_transport_reject, which cannot be expressed as one frame"
        ),
        ExecWsCommand::QueryOrders { .. } | ExecWsCommand::QueryFills { .. } => unreachable!(
            "queries never pass through dispatch_order; their transport \
             failures surface as errors from VenueQuery itself"
        ),
    }
}

/// Where an `OrderRejected` came from, which decides how much of the mirror it
/// is allowed to close.
///
/// The two origins carry different evidence. A `Venue` rejection is the venue's
/// own verdict, arriving over the wire or as an `AdmissionRejected` the venue
/// sent: whatever it says about the order is true, and the only question left is
/// whether nautilus's order state machine can express it. A `LocalTransport`
/// rejection is synthesized here for a command whose frame we believe never
/// left - and that belief is a guess, not a fact. `run_ws_connection`'s receipt
/// book documents the ambiguous window in as many words: bytes can reach the
/// venue before the writer task is aborted, and the retained receipt is reported
/// undelivered anyway.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RejectOrigin {
    Venue,
    LocalTransport,
}

/// The mirror statuses from which a submit rejection may be applied, given
/// where the rejection came from.
///
/// This is deliberately an enumeration rather than `!status.is_closed()`. The
/// two sets are not the same, and reading the negation as "safe" is how the
/// regression this replaced shipped:
///
/// - Against nautilus's own state machine (`OrderStatus::transition` in
///   `model/src/orders/mod.rs` of the pinned release), `OrderEventAny::Rejected`
///   has an arm from exactly `Initialized`, `Submitted`, `Accepted`,
///   `Triggered`, `PendingUpdate` and `PendingCancel`. `PartiallyFilled`,
///   `Emulated` and `Released` are all open by `is_closed`, and every one of
///   them answers `InvalidStateTransition`. So the old predicate admitted three
///   statuses whose event nautilus would refuse and whose mirror row it would
///   close anyway - a regression the mirror could not heal, since the mirror is
///   the reconciliation truth source.
/// - `Accepted` and `Triggered` stay in the `Venue` set on purpose, and this is
///   not an oversight to be tightened later. `mogwai_engine`'s `orders.rs`
///   genuinely rejects a resting order after acceptance: a post-only stop-limit
///   that becomes marketable is closed with `OrderRejected` at that moment.
///   Nautilus's own table marks the same arm "StopLimit order". Dropping those
///   would discard a real verdict.
/// - A `LocalTransport` rejection admits only `Initialized` and `Submitted` -
///   the statuses in which the venue has confirmed nothing about the order. If
///   the mirror has seen an accept, a trigger, a partial fill or a pending
///   amendment, the venue demonstrably received the submit, so "the frame never
///   left" is already known to be false and the synthesized rejection is the
///   wrong half of the ambiguous window. Refusing it leaves the order where the
///   venue's own events put it, which is the truth.
const fn admits_submit_rejection(status: OrderStatus, origin: RejectOrigin) -> bool {
    match origin {
        RejectOrigin::Venue => matches!(
            status,
            OrderStatus::Initialized
                | OrderStatus::Submitted
                | OrderStatus::Accepted
                | OrderStatus::Triggered
                | OrderStatus::PendingUpdate
                | OrderStatus::PendingCancel
        ),
        RejectOrigin::LocalTransport => {
            matches!(status, OrderStatus::Initialized | OrderStatus::Submitted)
        }
    }
}

/// The prefix a retryable venue refusal's reason carries, so a consumer can
/// tell backpressure from a business rejection without reading prose.
///
/// This exists because nautilus's `OrderRejected` has one free field - the
/// reason string - and both kinds of refusal arrive through it. Without a
/// marker a consumer's only lever is matching the venue's own sentence, which
/// makes its quarantine path's safety depend on wording nobody promised to
/// keep. This is that promise: a public constant, prepended by this adapter and
/// pinned by a test, so a consumer matches an identifier rather than a phrase.
///
/// What it means is exactly what the wire's `retryable` says: the venue was
/// full, not that it said no, and the same command sent later could succeed. It
/// says nothing about whether retrying is wise - that is the consumer's
/// judgement, and a consumer that prefers to stop when the venue said no is
/// still right to. Absent the prefix, a rejection is terminal and must be
/// treated as such.
///
/// Chosen to be unmistakable in a reason string and safe to match on a plain
/// `starts_with`.
pub const RETRYABLE_REJECT_PREFIX: &str = "[retryable] ";

/// Prepend [`RETRYABLE_REJECT_PREFIX`] to a refusal the venue called retryable,
/// leaving the venue's own reason after it so an operator reading logs still
/// learns which refusal it was.
fn mark_retryable(reason: &str, retryable: bool) -> String {
    if retryable {
        format!("{RETRYABLE_REJECT_PREFIX}{reason}")
    } else {
        reason.to_owned()
    }
}

/// The `order_list_id` a dispatched group is remembered under: any leg's link,
/// not leg 0's.
///
/// A nautilus order list need not link every member, so a list whose first leg
/// carries no link but whose later legs do is a perfectly ordinary shape. This
/// used to read `orders.first()` alone, which left such a list unremembered -
/// and an `AdmissionSubject::SubmitGroup` refusal for it then attributed to no
/// leg at all, so every member sat waiting on an answer the venue had already
/// given. A group is admitted or refused whole and its members share one list
/// id, so the first link that exists is the group's id whichever leg carries it.
fn group_id_of<'a>(links: impl IntoIterator<Item = Option<&'a str>>) -> Option<String> {
    links.into_iter().flatten().next().map(ToOwned::to_owned)
}

/// Reports a command the websocket accepted and never wrote - see
/// `lifecycle::run_ws_connection`'s `on_undelivered`.
///
/// An `Ok` from the command channel means queued, not sent, so a socket that
/// dies with frames still in the writer's queue swallows them: the order side
/// wedges in `Submitted`/`PendingUpdate` with no terminal event, and a query's
/// caller waits out its whole timeout for a reply nobody will ever send.
///
/// Order commands take the same synthesized reject as any other transport
/// failure. Queries cannot: `reject_for` has no shape for them, and the right
/// report is to retire the waiter, which drops its oneshot sender and makes
/// `await_reply` fail fast with "venue query abandoned" instead of parking for
/// the full request timeout. Retiring a slot that is already gone is a no-op,
/// which is what makes this safe to call for a query whose reply raced in.
fn report_undelivered_command(cmd: &ExecWsCommand, ctx: &ExecContext) {
    let err = anyhow::anyhow!(
        "the venue websocket closed before this command was written; it never reached the venue"
    );
    match cmd {
        ExecWsCommand::QueryOrders { request_id, .. } => {
            drop(
                lock_recover(&ctx.pending, "pending queries")
                    .orders
                    .remove(request_id),
            );
        }
        ExecWsCommand::QueryFills { request_id, .. } => {
            drop(
                lock_recover(&ctx.pending, "pending queries")
                    .fills
                    .remove(request_id),
            );
        }
        _ => synthesize_transport_reject(cmd, &err, ctx),
    }
}

/// Synthesizes the nautilus reject for a command whose transport failed before
/// the venue ever saw it - shared by `dispatch_order`'s send failure and the
/// reader's undelivered-command report (AE9). A failed `Cancel` is reported as a `CancelRejected`
/// (the order is still live, or its fate is simply unknown, not dead), leaving
/// the mirrored status untouched; a failed `Submit`/`Modify` is reported as the
/// matching `OrderRejected`/`OrderModifyRejected` so the order reaches a terminal
/// state instead of wedging in `Submitted`/`PendingUpdate`. Both bypass the
/// per-dispatch `HavocFilter` by design: the failure is purely local and never
/// traveled the wire, so there is nothing for the venue-havoc pipeline to model,
/// and routing a terminal reject through a `drop_prob` draw could discard it
/// entirely, leaving nautilus and the mirror stuck forever.
///
/// The submit rejection carries `RejectOrigin::LocalTransport`, which is what
/// keeps "the frame never left" from overruling evidence that it did. The
/// receipt book's window is ambiguous by construction - the bytes may have
/// reached the venue before the writer was aborted - so a mirror row that has
/// already seen an accept, a trigger, a partial fill or a pending amendment is
/// proof the submit landed, and the synthesized rejection is refused there
/// rather than closing an order the venue still owns. What remains wedged in
/// that case is nothing: the order is live at the venue and its own events
/// resolve it.
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
    } else if let ExecWsCommand::SubmitGroup(orders) = cmd {
        // Every leg, because the frame carrying them all never left. A group is
        // dispatched as one send, so its transport failure is one failure with
        // as many wedged orders as it had members, and rejecting one would
        // leave the rest in `Submitted` forever.
        let ts_event = now_unix_nanos(ctx.sim).as_u64();
        for order in orders {
            handle_exec_message_from(
                VenueMessage::OrderRejected {
                    client_order_id: order.client_order_id.clone(),
                    reason: err.to_string(),
                    ts_event,
                },
                ctx,
                RejectOrigin::LocalTransport,
            );
        }
    } else {
        handle_exec_message_from(
            reject_for(cmd, err, ctx.sim),
            ctx,
            RejectOrigin::LocalTransport,
        );
    }
}

/// Reports a rejected `Cancel` as a nautilus `OrderCancelRejected` without
/// touching the mirrored order's status. Serves both origins:
///
/// - a `Cancel` that failed at transport (its frame never reached the
///   venue): `ts_event` is sim-now and `wire_venue_order_id` is `None`, and
/// - a venue-originated `VenueMessage::OrderCancelRejected` (the engine could
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
        // Same limitation as the OrderRejected and OrderModifyRejected arms:
        // the mirror lacks the order, so we have no real strategy_id/
        // instrument_id, and a placeholder would be silently dropped by
        // nautilus `Order.apply` strategy-id validation. Make the drop
        // visible rather than silent.
        tracing::warn!(
            order = %client_order_id,
            reason = %reason,
            "cancel rejected for an order the mirror does not know; \
             reject not surfaced to nautilus"
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
    /// Snapshots must apply in venue order, not arrival order: nautilus applies
    /// account states in arrival order with no staleness guard of its own, so
    /// an older snapshot delivered late by reorder or duplicate havoc would
    /// overwrite newer balances and stay wrong until the next fill-driven
    /// snapshot - which may never come. `handle_account_state` skips any
    /// snapshot below this watermark.
    ///
    /// There is deliberately no position mirror behind this watermark any more.
    /// One existed to serve `generate_position_status_reports`, and it was that
    /// generator's only reader; since the generator now pulls venue truth from
    /// `GET /account`, a adapter-side copy could only ever be a second, staler
    /// answer to a question the venue already answers - and mogwai's own
    /// `DropNextAccountUpdate` divergence exists to make exactly that copy
    /// wrong.
    account_ts_last: UnixNanos,
    /// Which legs went out under which `order_list_id`, so a group refusal that
    /// names only the list can be answered per leg.
    ///
    /// Bounded and FIFO rather than a growing map: the entry is wanted for the
    /// round trip between a group's dispatch and the venue's answer, which is
    /// one command's worth of time, and a long forward run must not accumulate
    /// one row per bracket it ever sent. Losing an old row costs a refusal's
    /// legibility and nothing else - the `error` line still names the list.
    groups: std::collections::VecDeque<(String, Vec<ClientOrderId>)>,
    /// Map size at which the unbounded-growth `warn` next fires - see `prune`.
    /// Doubles each time it fires, so a genuinely large book says so once per
    /// doubling instead of once per insert.
    growth_warned_at: usize,
}

/// How many dispatched order groups are remembered for refusal attribution.
/// Generous for the in-flight window it actually covers, which is one round
/// trip, and small enough that the memory is a rounding error.
const REMEMBERED_GROUPS: usize = 64;

impl ExecState {
    fn remember_group(&mut self, order_list_id: String, members: Vec<ClientOrderId>) {
        self.groups.retain(|(id, _)| id != &order_list_id);
        self.groups.push_back((order_list_id, members));
        while self.groups.len() > REMEMBERED_GROUPS {
            self.groups.pop_front();
        }
    }

    /// Decides an arriving account snapshot against the staleness watermark and
    /// moves the watermark. Returns whether the snapshot should be forwarded.
    ///
    /// Nautilus applies account states in arrival order with no staleness guard
    /// of its own, so an older snapshot delivered late by reorder or duplicate
    /// havoc would overwrite newer balances and stay wrong until the next
    /// fill-driven one, which may never come. Anything below the watermark is
    /// therefore refused; equal-ts duplicates pass and re-apply idempotently.
    ///
    /// The watermark advances only over a snapshot that arrived whole - the
    /// frontier rule, that a cursor may not advance over work whose success the
    /// same expression did not check. `handle_account_state` builds its balances
    /// and margins with `filter_map`s that drop a row they cannot represent (an
    /// unknown currency, an amount that will not fit `Money`, a `locked + free
    /// != total` that `AccountBalance` refuses), each a warning rather than a
    /// failure. Advancing unconditionally meant a snapshot that lost half its
    /// balances still retired every earlier one, and a well-formed snapshot
    /// arriving late was then refused as stale while the account row kept the
    /// degraded state.
    fn admit_account_snapshot(&mut self, ts_event: UnixNanos, whole: bool) -> bool {
        if ts_event < self.account_ts_last {
            tracing::warn!(
                ts_event = ts_event.as_u64(),
                last_applied = self.account_ts_last.as_u64(),
                "dropping stale account snapshot: older than the last applied one"
            );
            return false;
        }
        if whole {
            self.account_ts_last = ts_event;
        }
        true
    }

    /// The group's legs, left in place.
    ///
    /// This used to remove the entry, which made a duplicated refusal
    /// unattributable - and duplicates are not hypothetical here,
    /// `duplicate_prob` exists to produce them. The first copy consumed the row
    /// and rejected every leg; the second found nothing and took the "cannot
    /// attribute" error path, which reads exactly like a real attribution
    /// failure.
    ///
    /// Leaving the row costs nothing: the ring is already bounded by
    /// `REMEMBERED_GROUPS` and a re-submitted list id is replaced by
    /// `remember_group`, so removal was an early free rather than the bound.
    /// The duplicate rejection it now permits is absorbed by
    /// `admits_submit_rejection`, which `handle_exec_message_from`'s
    /// `OrderRejected` arm consults: `Rejected` is not in either origin's
    /// admitted set, so the second copy leaves the record alone and emits
    /// nothing - which is where a duplicate belongs, rather than in an error
    /// log.
    fn peek_group(&self, order_list_id: &str) -> Option<Vec<ClientOrderId>> {
        self.groups
            .iter()
            .find(|(id, _)| id == order_list_id)
            .map(|(_, members)| members.clone())
    }
}

/// Cap on retained terminal order records. Open orders are never pruned (they
/// are live reconciliation truth); only closed records beyond this many are
/// dropped, oldest-by-`ts_last` first, so a long forward run cannot
/// accumulate terminal orders without bound (AE6). (The mirror once kept an
/// append-only fill Vec with its own cap; fill reports now come from the
/// venue-truth `QueryFills`, so no fill store remains to bound.)
const MAX_TERMINAL_ORDERS: usize = 10_000;

impl ExecState {
    /// Prunes the oldest terminal order records past `MAX_TERMINAL_ORDERS`.
    /// Called after each mirror mutation that can grow the map (a submit
    /// insert), and does real work only when the cap is exceeded.
    ///
    /// This does not bound the map, and the AE6 note that said it did was
    /// wrong. Open orders are never pruned, because they are live
    /// reconciliation truth and an adapter that forgot one would report a
    /// standing order as unknown - a strictly worse failure than memory. So a
    /// run that accumulates open (or permanently-`Submitted`) records grows
    /// linearly no matter what this function does, and the only honest
    /// response is to say so where an operator can see it. That is the `warn`
    /// below, raised on each doubling past the cap rather than per insert.
    ///
    /// The main source of permanent `Submitted` strays was a command the
    /// socket swallowed without a terminal event, which
    /// `lifecycle::run_ws_connection`'s undelivered report now closes: those
    /// records reach `Rejected` and become prunable. What remains is genuinely
    /// open orders, which is a strategy's business and not a leak.
    fn prune(&mut self) {
        if self.orders.len() > self.growth_warned_at.max(MAX_TERMINAL_ORDERS) {
            self.growth_warned_at = self.orders.len().saturating_mul(2);
            let open = self
                .orders
                .values()
                .filter(|record| !record.status.is_closed())
                .count();
            tracing::warn!(
                orders = self.orders.len(),
                open,
                cap = MAX_TERMINAL_ORDERS,
                "the execution mirror holds more orders than the terminal-record cap and most \
                 cannot be pruned; open records are live reconciliation truth and are never \
                 dropped, so this map grows with the number of orders left open"
            );
        }
        self.prune_terminal();
    }

    fn prune_terminal(&mut self) {
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

/// Applies a venue-originated message to the mirror and to nautilus.
///
/// Every caller but `synthesize_transport_reject` is holding something the venue
/// actually sent, so this is the ordinary entry point;
/// `handle_exec_message_from` is the one that takes a different origin.
fn handle_exec_message(msg: VenueMessage, ctx: &ExecContext) {
    handle_exec_message_from(msg, ctx, RejectOrigin::Venue);
}

fn handle_exec_message_from(msg: VenueMessage, ctx: &ExecContext, reject_origin: RejectOrigin) {
    match msg {
        VenueMessage::OrderTriggered { client_order_id, venue_order_id, ts_event } => {
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
        VenueMessage::OrderAccepted {
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
                // Accepted after the fill or cancel that ended the order (the
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
                // Same limitation as the reject arms: the mirror lacks the
                // order (e.g. an event arriving after reset() cleared it), so
                // there is no real strategy_id/instrument_id to emit with. Make
                // the drop visible instead of silent.
                tracing::warn!(
                    order = %client_order_id,
                    venue_order_id = %venue_order_id,
                    "order accepted for an order the mirror does not know; \
                     event not surfaced to nautilus"
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
        VenueMessage::OrderRejected {
            client_order_id,
            reason,
            ts_event,
        } => {
            let Some(client_order_id) = wire_client_order_id(&client_order_id) else {
                return;
            };
            let Some((record, stale)) = with_order_record(&ctx.state, client_order_id, |record| {
                // Refusals can be synthesized by admission and transport paths,
                // and reorder havoc can put either behind a later lifecycle
                // event, so this rejection may well be describing an order the
                // mirror has already moved past. `admits_submit_rejection`
                // enumerates the statuses this rejection may close, per its
                // origin; everything else keeps the status it has and emits
                // nothing, because the mirror is the reconciliation truth
                // source and nautilus's own FSM would refuse the event anyway.
                let stale = !admits_submit_rejection(record.status, reject_origin);
                if !stale {
                    record.status = OrderStatus::Rejected;
                    record.ts_last = record.ts_last.max(UnixNanos::from(ts_event));
                }
                (record.clone(), stale)
            }) else {
                // The local mirror does not know this order, so we lack the
                // real strategy_id/instrument_id the emit requires. We cannot
                // synthesize them: nautilus `Order.apply` hard-validates the
                // event's strategy_id against the cached order and silently
                // drops the event on mismatch, so a placeholder would guarantee
                // the drop rather than surface the reject. Surfacing it
                // correctly would mean resolving the order from the nautilus
                // cache, which ExecContext does not hold - a design change, not
                // a local fix. For now make the drop visible instead of silent.
                tracing::warn!(
                    order = %client_order_id,
                    reason = %reason,
                    "order rejected for an order the mirror does not know; \
                     reject not surfaced to nautilus"
                );
                return;
            };
            if stale {
                tracing::warn!(
                    order = %client_order_id,
                    status = ?record.status,
                    origin = ?reject_origin,
                    "rejection refused by the mirror: this status does not admit \
                     a submit rejection from this origin; keeping it"
                );
                return;
            }
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
        VenueMessage::OrderCanceled {
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
                // Same limitation as the reject arms: no real strategy_id/
                // instrument_id to emit with. Make the drop visible.
                tracing::warn!(
                    order = %client_order_id,
                    venue_order_id = %venue_order_id,
                    "order canceled for an order the mirror does not know; \
                     event not surfaced to nautilus"
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
        // Deliberately a copy of the Canceled arm rather than a shared helper
        // over both: the two are the same shape carrying different facts, and the
        // pressure to fold them is what collapsed expiry into cancellation in
        // the first place. Everything below - the terminal-state guard, the
        // forward-only `ts_last`, the unknown-order warning - is the same
        // rule for the same reasons; only the status and the emitted event
        // differ.
        VenueMessage::OrderExpired {
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
                let stale = record.status.is_closed();
                if !stale {
                    record.status = OrderStatus::Expired;
                    record.venue_order_id = Some(venue_order_id);
                    record.ts_last = record.ts_last.max(UnixNanos::from(ts_event));
                }
                (record.clone(), stale)
            }) else {
                tracing::warn!(
                    order = %client_order_id,
                    venue_order_id = %venue_order_id,
                    "order expired for an order the mirror does not know; \
                     event not surfaced to nautilus"
                );
                return;
            };
            if stale {
                tracing::warn!(
                    order = %client_order_id,
                    status = ?record.status,
                    "expired event for a terminal mirror record; keeping the \
                     terminal status (reordered or duplicated event)"
                );
            }
            let event = OrderExpired::new(
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
            ctx.emitter.send_order_event(OrderEventAny::Expired(event));
        }
        VenueMessage::OrderUpdated {
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
                // Same limitation as the reject arms: no real strategy_id/
                // instrument_id to emit with. Make the drop visible.
                tracing::warn!(
                    order = %client_order_id,
                    venue_order_id = %venue_order_id,
                    "order update for an order the mirror does not know; \
                     event not surfaced to nautilus"
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
                    // non-zero filled_qty stays `PartiallyFilled` (or flips to `Filled`
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
        VenueMessage::OrderModifyRejected {
            client_order_id,
            venue_order_id,
            reason,
            ts_event,
        } => {
            let Some(client_order_id) = wire_client_order_id(&client_order_id) else {
                return;
            };
            let Some(record) = order_record(&ctx.state, client_order_id) else {
                // Same limitation as the OrderRejected arm: the mirror lacks the
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
                     reject not surfaced to nautilus"
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
        VenueMessage::OrderCancelRejected {
            client_order_id,
            venue_order_id,
            reason,
            ts_event,
        } => {
            // A venue-originated cancel rejection. emit_cancel_rejected leaves
            // the mirror's status untouched (nautilus restores the pre-cancel
            // status) and handles the unknown-order drop, exactly as the
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
        VenueMessage::OrderFilled(fill) => handle_order_filled(&fill, ctx),
        // Venue-truth query replies: resolve the waiter registered under the
        // echoed correlation id. A reply with no waiter is a straggler whose
        // requester already timed out (inbound havoc delayed it past the
        // request timeout) or a duplicate - log it and move on; the content
        // was truthful either way, only the delivery was havoc'd.
        VenueMessage::OrderStatusSnapshot(snapshot) => {
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
        VenueMessage::FillSnapshot(snapshot) => {
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
        VenueMessage::AccountState(state) => handle_account_state(&state, ctx),
        VenueMessage::Heartbeat { .. } => {
            tracing::trace!("ignoring venue heartbeat on execution path");
        }
        VenueMessage::ProtocolError { reason, .. } => {
            // Untargeted (no client_order_id to attribute it to, unlike
            // OrderRejected), so there is no nautilus order event to raise -
            // just make the venue-side decode failure visible in the adapter's
            // own logs.
            tracing::warn!(%reason, "venue reported a protocol error");
        }
        VenueMessage::AdmissionRejected {
            subject,
            reason,
            retryable,
            ts_event,
        } => {
            // The marker itself, and it is the whole reason `retryable` is on the wire.
            //
            // Nautilus's `OrderRejected` carries a reason string and nothing
            // else this adapter may set, so an admission refusal and a business
            // rejection ("insufficient balance", "market closed") arrive at a
            // strategy in the same shape. A consumer that wanted to treat
            // backpressure as retryable had only the venue's wording to key on,
            // and correctly refused to hang a quarantine decision on our prose.
            //
            // So the wording stops being prose and becomes a contract: a
            // retryable refusal's reason is prefixed with
            // `RETRYABLE_REJECT_PREFIX`, that prefix is a public constant of
            // this crate, and `a_retryable_admission_refusal_is_marked_for_the_consumer`
            // pins it. A consumer matches the constant, not a sentence. The
            // venue's own reason follows it unchanged, so an operator reading
            // logs still learns which refusal it was.
            let reason = mark_retryable(&reason, retryable);
            match subject {
            mogwai_protocol::AdmissionSubject::Submit { client_order_id } => {
                handle_exec_message(
                    VenueMessage::OrderRejected {
                        client_order_id,
                        reason,
                        ts_event,
                    },
                    ctx,
                );
            }
            mogwai_protocol::AdmissionSubject::SubmitGroup { order_list_id } => {
                // The venue refused the whole group and named the list, because
                // a group is admitted or refused whole and naming one member
                // would be as wrong as naming none. Fan it back out to every
                // leg: nautilus has no order-list-scoped rejection event, so a
                // leg that got no answer would sit in the strategy's book
                // forever waiting on one.
                let members = lock_recover(&ctx.state, "exec state").peek_group(&order_list_id);
                match members {
                    Some(members) if !members.is_empty() => {
                        for client_order_id in members {
                            handle_exec_message(
                                VenueMessage::OrderRejected {
                                    client_order_id: client_order_id.to_string(),
                                    reason: reason.clone(),
                                    ts_event,
                                },
                                ctx,
                            );
                        }
                    }
                    // The dispatch record aged out of the bounded ring, or this
                    // client never sent that list. Nothing can be attributed,
                    // so say so loudly rather than swallow a refusal.
                    _ => tracing::error!(
                        %order_list_id,
                        %reason,
                        "venue refused an order group this client cannot attribute to its legs; \
                         those orders will not receive a rejection"
                    ),
                }
            }
            mogwai_protocol::AdmissionSubject::Cancel { client_order_id } => {
                handle_exec_message(
                    VenueMessage::OrderCancelRejected {
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
                    VenueMessage::OrderModifyRejected {
                        client_order_id,
                        venue_order_id: None,
                        reason,
                        ts_event,
                    },
                    ctx,
                );
            }
            mogwai_protocol::AdmissionSubject::Query { request_id, query } => {
                // Drop the waiter rather than answer it. An empty snapshot
                // would be a false venue truth - "you have no orders" when the
                // venue in fact never looked - and the mirror would reconcile
                // against it. Dropping the oneshot sender wakes the requester
                // with a `RecvError` immediately, exactly as a disconnect does,
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
                    // History waiters live on the data leg, which owns the
                    // river and is the only half that asks for a page. An
                    // execution socket holds no such waiter, so there is
                    // nothing here to wake - and probing the order or fill maps
                    // for a history id could wake the wrong waiter, which is
                    // the exact collision the discriminator exists to prevent.
                    mogwai_protocol::QueryKind::HistoryTrades
                    | mogwai_protocol::QueryKind::HistoryQuotes => false,
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
                // A whole outbound batch the venue discarded, so the events it
                // carried are simply gone. Same severity and same wording as
                // the `FeedLagged` arm below, and for the same reason: the
                // mirror may now disagree with venue truth and only a host-
                // driven reconciliation can settle it. Nothing here can
                // trigger that; see the cross-repo item in notes/todo.md.
                tracing::error!(
                    ?subject,
                    %reason,
                    "venue refused a whole execution frame; order events may be missing and the mirror should be reconciled against venue truth"
                );
            }
            }
        }
        VenueMessage::FeedLagged {
            episode,
            skipped,
            skipped_total,
            ..
        } => {
            // Never an execution signal, and it used to be logged as one. This
            // frame reports that the boat's market ring overwrote frames before
            // this socket read them. Execution output never travels that ring:
            // it is queued on the held lane and pumped into the same writer, so
            // a ring overrun cannot drop, reorder or overwrite an order event.
            // Claiming the mirror might disagree with venue truth here asked a
            // host to reconcile a book that was never in doubt.
            //
            // The venue does have a signal for genuinely lost execution output,
            // and it is the `AdmissionSubject::Frame` arm above: a whole
            // outbound batch the venue refused. That one means what this one was
            // saying.
            //
            // `warn` rather than `error`: this socket consumes the market tape only
            // to discard it, so the loss costs the execution leg nothing. What
            // it indicates is that this connection is not draining fast enough,
            // which is worth knowing and is not a fault.
            tracing::warn!(
                episode,
                skipped,
                skipped_total,
                "venue declared a market-view gap on the execution socket; the execution stream is unaffected and no reconciliation is owed"
            );
        }
        // Market data is handled by the data client, and so is history: the
        // execution leg never asks for a page, so one arriving here would be a
        // reply to a request this half did not make.
        VenueMessage::Trade(_)
        | VenueMessage::Quote(_)
        | VenueMessage::HistoryPage { .. }
        | VenueMessage::HistoryRejected { .. }
        | VenueMessage::HavocDiagnostic { .. }
        // A clean completion is a transport concern, whether the RUN ended or
        // this passenger's own duration did. The reader owns reconnect policy;
        // the execution event translator has no event to publish for either.
        | VenueMessage::RunComplete { .. }
        | VenueMessage::PassengerDurationComplete { .. } => {}
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
    // engine's ids are short, but this is a wire value: a venue bug or havoc
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
    // fill reports now come from the venue-truth QueryFills rather than any
    // mirror fill store, so nothing outside the closure branches on it.
    let Some((record, _is_duplicate)) = with_order_record(&ctx.state, client_order_id, |record| {
        let is_duplicate = !record.seen_trades.insert(trade_id);
        if !is_duplicate {
            // Terminal-state guard (see the OrderAccepted arm): a partial fill
            // transposed behind the cancel (or the final fill) that ended the
            // order must still book its economics - money moved at the venue,
            // but must not regress the terminal status back to `PartiallyFilled`
            // and re-open a closed order in the reconciliation mirror.
            if !record.status.is_closed() {
                record.status = if fill.leaves_qty.is_zero() {
                    OrderStatus::Filled
                } else {
                    OrderStatus::PartiallyFilled
                };
            }
            record.venue_order_id = Some(venue_order_id);
            // A reordered fill carries an older ts_event than the event that
            // already advanced the record; only ever move ts_last forward so
            // the mirror's in_time_range filtering does not walk backward.
            record.ts_last = record.ts_last.max(UnixNanos::from(fill.ts_event));
        }
        (record.clone(), is_duplicate)
    }) else {
        // The worst silent drop on this path: money moved at the venue and no
        // nautilus event can be built (same limitation as the reject
        // arms - the mirror lacks the order, so there is no real strategy_id/
        // instrument_id to emit with). Make it loud.
        tracing::warn!(
            order = %client_order_id,
            trade = %trade_id,
            venue_order_id = %venue_order_id,
            "order fill for an order the mirror does not know; \
             fill not surfaced to nautilus"
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
    // The wire's account id is deliberately not compared against the configured
    // one here, and the difference matters more on this path than on the connect
    // path: this used to drop a snapshot whose label differed, so a venue and a
    // client that named the account differently produced a client whose balances
    // silently stopped updating while every fill still arrived.
    //
    // The drop guarded against adopting a misrouted snapshot back when the
    // account was an addressable slot a venue could route one to the wrong one
    // of. The scope that survives is the connection, not the venue: a venue does
    // hold several ledgers, one per account id, but a socket names
    // exactly one on its `/ws?account=` upgrade and only that one's state comes
    // back down it. So there is nothing to be misrouted from on this path, and a dropped
    // snapshot can only lose state that was correct. The configured id is
    // stamped on below, as it always was; `note_account_label` says once at
    // connect if the two names differ. `reference/architecture.md` carries the
    // argument in full, including what would have to change first if a socket
    // ever carried several ledgers.
    let ts_event = UnixNanos::from(state.ts_event);
    let balances: Vec<AccountBalance> = state
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
    let margins: Vec<MarginBalance> = state
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
        // its own, so an older snapshot delivered late by reorder/duplicate
        // havoc would overwrite newer balances and stay wrong until the next
        // fill-driven snapshot, which may be never. Skip any snapshot below the
        // applied watermark. Equal-ts duplicates pass; they re-apply
        // idempotently.
        // A degraded snapshot is still forwarded - a partial account view beats
        // none, and it is what the venue said - but it does not claim the
        // ground a whole one would. See `admit_account_snapshot`.
        let whole = balances.len() == state.balances.len() && margins.len() == state.margins.len();
        if !mirror.admit_account_snapshot(ts_event, whole) {
            return;
        }
        if !whole {
            tracing::warn!(
                ts_event = ts_event.as_u64(),
                balances_kept = balances.len(),
                balances_sent = state.balances.len(),
                margins_kept = margins.len(),
                margins_sent = state.margins.len(),
                "forwarding a degraded account snapshot without advancing the staleness \
                 watermark, so a well-formed snapshot for an earlier instant can still supersede it"
            );
        }
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

/// Converts a venue-sent `client_order_id` string into a nautilus
/// `ClientOrderId`, dropping the event with a warning instead of panicking.
/// `ClientOrderId::from` routes through the panicking `new`, which nautilus's
/// `check_valid_string_ascii` rejects on an empty, whitespace-only, or
/// non-ASCII string (there is no length cap, unlike `TradeId`). These are wire
/// values, so a venue bug or havoc corruption sending a malformed id must not
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

/// Converts a venue-sent `venue_order_id` string into a nautilus
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

    fn test_client() -> MogwaiExecutionClient {
        let config = MogwaiExecClientConfig {
            base_url: "ws://127.0.0.1:1".into(),
            ..MogwaiExecClientConfig::default()
        };
        let core = ExecutionClientCore::new(
            config.trader_id,
            ClientId::from("MOGWAI-EXEC"),
            *MOGWAI_VENUE,
            OmsType::Netting,
            config.account_id,
            config.account_type,
            None,
            std::rc::Rc::new(std::cell::RefCell::new(
                nautilus_common::cache::Cache::default(),
            )),
        );
        MogwaiExecutionClient::new(core, config).expect("test client builds")
    }

    /// A mirror seeded with one order in `status`, plus the context that reads
    /// it. `ts_last` is 20 so a rejection stamped earlier can be seen to leave
    /// it alone.
    fn a_mirrored_order(status: OrderStatus) -> (Arc<Mutex<ExecState>>, ExecContext) {
        let state = Arc::new(Mutex::new(ExecState::default()));
        lock_recover(&state, "test state").orders.insert(
            ClientOrderId::from("O-1"),
            OrderRecord {
                strategy_id: nautilus_model::identifiers::StrategyId::from("S-1"),
                instrument_id: InstrumentId::from("EURUSD.MOGWAI"),
                order_side: OrderSide::Buy,
                order_type: OrderType::Limit,
                status,
                venue_order_id: Some(VenueOrderId::from("V-1")),
                ts_last: UnixNanos::from(20),
                seen_trades: std::collections::HashSet::new(),
            },
        );
        let trader_id = nautilus_model::identifiers::TraderId::from("TRADER-001");
        let account_id = AccountId::from("MOGWAI-001");
        let ctx = ExecContext {
            emitter: ExecutionEventEmitter::new(
                get_atomic_clock_realtime(),
                trader_id,
                account_id,
                nautilus_model::enums::AccountType::Margin,
                None,
            ),
            state: Arc::clone(&state),
            instruments: Arc::new(Mutex::new(HashMap::new())),
            pending: Arc::new(Mutex::new(PendingQueries::default())),
            trader_id,
            account_id,
            account_type: nautilus_model::enums::AccountType::Margin,
            sim: SimClock::identity(),
        };
        (state, ctx)
    }

    fn mirrored_status(state: &Arc<Mutex<ExecState>>) -> (OrderStatus, UnixNanos) {
        let state = lock_recover(state, "test state");
        let record = state
            .orders
            .get(&ClientOrderId::from("O-1"))
            .expect("order remains mirrored");
        (record.status, record.ts_last)
    }

    #[test]
    fn reset_clears_a_poisoned_execution_mirror() {
        let mut client = test_client();
        lock_recover(&client.state, "test state").orders.insert(
            ClientOrderId::from("O-OLD"),
            OrderRecord {
                strategy_id: nautilus_model::identifiers::StrategyId::from("S-1"),
                instrument_id: InstrumentId::from("BTCUSDT.MOGWAI"),
                order_side: OrderSide::Buy,
                order_type: OrderType::Limit,
                status: OrderStatus::Accepted,
                venue_order_id: Some(VenueOrderId::from("V-OLD")),
                ts_last: UnixNanos::from(20),
                seen_trades: std::collections::HashSet::new(),
            },
        );
        let state = Arc::clone(&client.state);
        let poison = std::thread::spawn(move || {
            let _guard = state.lock().expect("state starts healthy");
            panic!("poison the execution mirror");
        });
        assert!(poison.join().is_err(), "the poison thread must panic");

        client
            .reset()
            .expect("reset must recover the mirror lock and clear it");
        assert!(
            lock_recover(&client.state, "test state").orders.is_empty(),
            "the prior passenger's order must not survive reset"
        );
    }

    /// Every nautilus `OrderStatus`, against both rejection origins.
    ///
    /// Written as a full enumeration rather than a couple of interesting rows
    /// because the defect this replaced was precisely a set that read as
    /// complete: `!status.is_closed()` admitted `PartiallyFilled`, `Emulated`
    /// and `Released`, none of which nautilus's own FSM will transition to
    /// `Rejected`, and a Filled-only regression passed over all three. A new
    /// `OrderStatus` variant fails the match below until its row is written.
    #[test]
    fn a_submit_rejection_admits_only_the_statuses_its_origin_supports() {
        let statuses = [
            OrderStatus::Initialized,
            OrderStatus::Denied,
            OrderStatus::Emulated,
            OrderStatus::Released,
            OrderStatus::Submitted,
            OrderStatus::Accepted,
            OrderStatus::Rejected,
            OrderStatus::Canceled,
            OrderStatus::Expired,
            OrderStatus::Triggered,
            OrderStatus::PendingUpdate,
            OrderStatus::PendingCancel,
            OrderStatus::PartiallyFilled,
            OrderStatus::Filled,
            OrderStatus::Voided,
        ];
        let mut venue_admitted = Vec::new();
        let mut transport_admitted = Vec::new();
        for status in statuses {
            for (origin, admitted) in [
                (RejectOrigin::Venue, &mut venue_admitted),
                (RejectOrigin::LocalTransport, &mut transport_admitted),
            ] {
                let (state, ctx) = a_mirrored_order(status);
                handle_exec_message_from(
                    VenueMessage::OrderRejected {
                        client_order_id: "O-1".to_owned(),
                        reason: "late refusal".to_owned(),
                        ts_event: 10,
                    },
                    &ctx,
                    origin,
                );
                let (after, ts_last) = mirrored_status(&state);
                if after == OrderStatus::Rejected && status != OrderStatus::Rejected {
                    admitted.push(status);
                } else {
                    // A refused rejection leaves both fields exactly as it
                    // found them, including the backward `ts_event`.
                    assert_eq!(
                        (status, after, ts_last),
                        (status, status, UnixNanos::from(20)),
                        "{origin:?} rejection disturbed a {status:?} record"
                    );
                }
            }
        }
        assert_eq!(
            venue_admitted,
            [
                OrderStatus::Initialized,
                OrderStatus::Submitted,
                OrderStatus::Accepted,
                OrderStatus::Triggered,
                OrderStatus::PendingUpdate,
                OrderStatus::PendingCancel,
            ],
            "the venue set must be exactly nautilus's own Rejected arms"
        );
        assert_eq!(
            transport_admitted,
            [OrderStatus::Initialized, OrderStatus::Submitted],
            "a synthesized transport reject may only close an order the venue \
             has confirmed nothing about"
        );
    }

    /// The ambiguous-send window, with its interleaving forced rather than
    /// raced.
    ///
    /// `run_ws_connection` retains a receipt for a command it queued, and
    /// aborting the writer reports that receipt undelivered - but the bytes may
    /// already have reached the venue. Here they did: the venue's acceptance is
    /// applied first, and only then does the receipt book report the submit as
    /// never sent. Driving `report_undelivered_command`, not the arm beneath
    /// it, is what pins the origin actually threaded through
    /// `synthesize_transport_reject`.
    #[test]
    fn a_transport_reject_cannot_close_an_order_the_venue_accepted() {
        fn a_submit() -> mogwai_protocol::SubmitOrder {
            mogwai_protocol::SubmitOrder {
                client_order_id: "O-1".to_owned(),
                symbol: Symbol::from("EURUSD"),
                position_id: None,
                side: mogwai_protocol::Side::Buy,
                order_type: mogwai_protocol::OrderType::Limit,
                quantity: Decimal::ONE,
                price: Some(Decimal::ONE),
                trigger_price: None,
                trail_offset: None,
                limit_offset: None,
                reduce_only: false,
                post_only: false,
                time_in_force: mogwai_protocol::TimeInForce::Gtc,
                expire_time: None,
                link: None,
            }
        }

        let (state, ctx) = a_mirrored_order(OrderStatus::Submitted);
        handle_exec_message(
            VenueMessage::OrderAccepted {
                client_order_id: "O-1".to_owned(),
                venue_order_id: "V-1".to_owned(),
                ts_event: 30,
            },
            &ctx,
        );
        assert_eq!(mirrored_status(&state).0, OrderStatus::Accepted);
        report_undelivered_command(&ExecWsCommand::Submit(a_submit()), &ctx);
        assert_eq!(
            mirrored_status(&state),
            (OrderStatus::Accepted, UnixNanos::from(30)),
            "the venue owns this order; the receipt book's guess must not close it"
        );
        // The group fan-out reaches the same arm by its own path, so it owes
        // its own origin. A bracket whose frame was reported undelivered after
        // the venue accepted a leg must leave that leg alone too.
        report_undelivered_command(&ExecWsCommand::SubmitGroup(vec![a_submit()]), &ctx);
        assert_eq!(
            mirrored_status(&state),
            (OrderStatus::Accepted, UnixNanos::from(30)),
            "the group fan-out must carry the same origin as a bare submit"
        );
    }

    /// The contract a consumer matches on, pinned so it cannot drift into
    /// prose.
    ///
    /// Nautilus's `OrderRejected` gives this adapter one free field, so an
    /// admission refusal and a business rejection reach a strategy in the same
    /// shape. The prefix is what makes them separable by an identifier rather
    /// than by the venue's wording - which is what a consumer refused to hang
    /// its quarantine path on, correctly. Changing the constant is a breaking
    /// change to that contract, and this is where that shows up.
    #[test]
    fn a_retryable_admission_refusal_is_marked_for_the_consumer() {
        assert_eq!(RETRYABLE_REJECT_PREFIX, "[retryable] ");

        let marked = mark_retryable("venue command capacity exhausted", true);
        assert!(
            marked.starts_with(RETRYABLE_REJECT_PREFIX),
            "a consumer matches the prefix, not the sentence: {marked}"
        );
        assert!(
            marked.ends_with("venue command capacity exhausted"),
            "and the venue's own reason survives it, so an operator still \
             learns which refusal it was: {marked}"
        );

        // The negative, without which the assertion above holds for a marker
        // applied to everything: a rejection the venue did not call retryable
        // passes through untouched and stays terminal.
        let business = mark_retryable("insufficient balance", false);
        assert_eq!(business, "insufficient balance");
        assert!(!business.starts_with(RETRYABLE_REJECT_PREFIX));
    }

    /// A degraded account snapshot must not retire the well-formed ones.
    ///
    /// The frontier rule: the watermark may only advance over a snapshot every
    /// row of which survived conversion. Advancing unconditionally meant a
    /// snapshot that dropped rows (unknown currency, `locked + free != total`)
    /// still claimed its instant, and the well-formed snapshot that reorder
    /// havoc delivered a moment later for an earlier instant was then refused
    /// as stale - leaving the account row degraded until a newer fill produced
    /// another, which may never come.
    #[test]
    fn a_degraded_account_snapshot_does_not_advance_the_staleness_watermark() {
        let mut state = ExecState::default();
        assert!(state.admit_account_snapshot(UnixNanos::from(10), true));
        assert!(
            state.admit_account_snapshot(UnixNanos::from(20), false),
            "a degraded snapshot is still forwarded - a partial view beats none"
        );
        assert!(
            state.admit_account_snapshot(UnixNanos::from(15), true),
            "and the well-formed snapshot for an earlier instant is still admitted, \
             because the degraded one at 20 never claimed that ground"
        );
        assert!(
            !state.admit_account_snapshot(UnixNanos::from(14), true),
            "the whole snapshot at 15 did claim it, so anything older is stale - \
             without this the test would pass against a watermark that never moves"
        );
    }

    /// A duplicated group refusal must still name the legs.
    ///
    /// `duplicate_prob` and `DuplicateNextFill` exist to deliver the same frame
    /// twice, so a second `AdmissionRejected` for one list is an expected
    /// arrival, not a corruption. The lookup used to remove the row, which made
    /// the second copy unattributable and sent it down the "cannot attribute"
    /// error path - a log line that reads exactly like a real attribution
    /// failure, on a duplicate that was working as designed.
    #[test]
    fn a_group_refusal_can_be_attributed_twice() {
        let mut state = ExecState::default();
        state.remember_group(
            "L-1".to_string(),
            vec![ClientOrderId::from("O-1"), ClientOrderId::from("O-2")],
        );
        let first = state.peek_group("L-1").expect("the group is remembered");
        let second = state
            .peek_group("L-1")
            .expect("a duplicated refusal finds it too");
        assert_eq!(first, second);
        assert_eq!(first.len(), 2);
        assert!(
            state.peek_group("L-2").is_none(),
            "and a list this client never sent is still unattributable"
        );
    }

    /// A group is remembered by whichever leg's link comes first, not leg 0's.
    ///
    /// A nautilus order list need not link every member, so a list whose first
    /// leg is unlinked is ordinary. Keying attribution off `orders.first()`
    /// left that list unremembered, and a refusal for it then rejected not a single leg -
    /// it fell through to the "cannot attribute" error. The selection lives in
    /// `group_id_of` precisely so it can be bitten here; the submit path around
    /// it needs a live emitter and a socket.
    #[test]
    fn the_group_id_is_taken_from_whichever_leg_carries_the_link() {
        let unlinked_first = [None, Some("L-9".to_string()), Some("L-9".to_string())];
        assert_eq!(
            group_id_of(unlinked_first.iter().map(Option::as_deref)),
            Some("L-9".to_string()),
            "an unlinked first leg must not hide the group's id"
        );
        assert_eq!(
            group_id_of([None, None]),
            None,
            "and a list with no link at all is genuinely not a group"
        );
    }

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

    #[tokio::test]
    async fn a_targeted_query_discards_a_row_for_another_order() {
        let (ws_tx, mut ws_rx) = unbounded_channel();
        let pending = Arc::new(Mutex::new(PendingQueries::default()));
        let query = VenueQuery {
            ws_cmd: Some(ws_tx),
            pending: Arc::clone(&pending),
            timeout_secs: 1,
        };
        let task =
            tokio::spawn(
                async move { query.order_status(Some("O-WANTED".to_owned()), false).await },
            );
        let ExecWsCommand::QueryOrders { request_id, .. } =
            ws_rx.recv().await.expect("the query is dispatched")
        else {
            panic!("targeted order_status dispatched the wrong command kind")
        };
        let reply = lock_recover(&pending, "pending queries")
            .orders
            .remove(&request_id)
            .expect("the correlated waiter exists");
        reply
            .send(OrderStatusSnapshot {
                request_id,
                orders: vec![OrderStatusInfo {
                    client_order_id: mogwai_protocol::ClientOrderId::from("O-OTHER"),
                    venue_order_id: mogwai_protocol::VenueOrderId::from("V-1"),
                    symbol: Symbol::from("BTCUSDT"),
                    position_id: None,
                    side: mogwai_protocol::Side::Buy,
                    order_type: mogwai_protocol::OrderType::Limit,
                    time_in_force: mogwai_protocol::TimeInForce::Gtc,
                    status: mogwai_protocol::WireOrderStatus::Accepted,
                    quantity: Decimal::ONE,
                    filled_qty: Decimal::ZERO,
                    price: Some(Decimal::from(100)),
                    trigger_price: None,
                    ts_triggered: None,
                    reduce_only: false,
                    post_only: false,
                    ts_accepted: 1,
                    ts_last: 1,
                }],
                ts_event: 1,
            })
            .expect("the query task still owns the receiver");

        let snapshot = task
            .await
            .expect("query task joins")
            .expect("query succeeds");
        assert!(
            snapshot.orders.is_empty(),
            "the targeted query must not return O-OTHER as O-WANTED"
        );
    }
}
