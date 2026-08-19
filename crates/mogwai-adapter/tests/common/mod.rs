// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Shared test-support harness for the ignored adapter integration tests.
//!
//! All three adapter test binaries (`adapter_smoke`, `data_client_transport`,
//! `havoc`) drive the public adapter path against a self-contained HTTP +
//! WebSocket stub that speaks just enough of the mogwai protocol. The stub used
//! to be copy-pasted across the three files with subtle divergence (only the
//! havoc copy parsed `Content-Length`, `respond_json` had two different
//! signatures, `serve_ws` was fixed-frame in two and data-driven in one). This
//! module is the single, most-capable variant; each test still drives its own
//! scenario by mutating the shared [`StubState`] - the one thing that
//! legitimately differs between tests.
//!
//! The WS leg matches on a parsed [`ClientMessage`] rather than a type-name
//! substring, so a wire-format change (a field rename, an envelope change) makes
//! the stub stop recognising the client's `Subscribe` / `SubmitOrder` and the
//! test fails loudly instead of passing against a broken protocol.

// A shared test-support module: every item is `pub` so the three test binaries
// can use it, but nothing is reachable outside the crate, so both lints fire
// here by construction rather than flagging real issues.
#![allow(dead_code, unreachable_pub)]

use std::{
    cell::RefCell,
    collections::VecDeque,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use mogwai_adapter::{MOGWAI_VENUE, MogwaiExecClientConfig, MogwaiExecutionClient};
use mogwai_protocol::{
    ClientMessage, FillSnapshot, OrderFilled, OrderStatusInfo, OrderStatusSnapshot, ServerMessage,
    WireOrderStatus,
};
use nautilus_common::{
    cache::Cache,
    clients::ExecutionClient,
    clock::TestClock,
    factories::OrderFactory,
    messages::{DataEvent, ExecutionEvent},
};
use nautilus_live::ExecutionClientCore;
use nautilus_model::{
    enums::{OmsType, OrderSide, TimeInForce, TrailingOffsetType, TriggerType},
    identifiers::{AccountId, ClientId, ClientOrderId, InstrumentId, StrategyId, Symbol, TraderId},
    types::{Price, Quantity},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc::UnboundedReceiver,
};
use tokio_tungstenite::tungstenite::Message;

/// The canonical single-instrument `/instruments` seed both ends agree on.
pub const INSTRUMENTS_JSON: &str = r#"[{"symbol":"BTCUSDT","class":{"class":"spot","base":"BTC","quote":"USDT"},"price_precision":2,"size_precision":8,"price_increment":"0.01","size_increment":"0.00000001"}]"#;

/// Stable non-zero instant stamped on venue-truth query envelopes.
pub const VENUE_SNAPSHOT_TS_EVENT: u64 = 1_000_000_000;

/// Test scenario state shared between the stub and the test body. A test mutates
/// the relevant fields before connecting, then reads the counters/recorded
/// bodies afterwards. Defaults model a clean, honest venue.
///
/// THE AXIS THAT MATTERS HERE IS DATA-LEG VERSUS EXEC-LEG, NOT HTTP VERSUS WS,
/// and it is written down because getting it wrong is how the dead block in
/// `serve_exec_message`'s doc comment survived. Splitting this struct by
/// transport would have put `ws_trades`, `dark_ms`, `close_after_trades`,
/// `ws_server_pings`, `ws_exec_frames` and `ws_modify_frames` in ONE bucket
/// together - which is precisely the confusion that produced the defect, so
/// that split localizes nothing. The ownership is:
///
/// - DATA LEG, all of it served before the read loop in `serve_ws`:
///   `ws_trades`, `push_gate`, `dark_ms`, `close_after_trades`,
///   `ws_server_pings`, `ws_first_frame_at`.
/// - EXEC LEG, all of it served inside `serve_exec_message`: `ws_exec_frames`,
///   `ws_modify_frames`, `venue_orders`, `venue_fills`, `order_queries`,
///   `fill_queries`, `ws_first_exec_frame_at`.
/// - EITHER LEG: the handshake and socket bookkeeping (`ws_handshakes`,
///   `ws_hits`, `ws_requests`, `active_ws`, `refuse_ws`, `ws_pings`,
///   `ws_pongs`, `ws_client_messages`) and everything HTTP.
///
/// A NEW FIELD BELONGS IN EXACTLY ONE OF THOSE THREE, and a fixture that arms a
/// data-leg switch on a test whose client is an exec client is arming nothing:
/// exec clients never enter the tape push, data clients never send a
/// `ClientMessage`.
#[derive(Default)]
pub struct StubState {
    /// Optional body served by `/instruments`; absent uses BTCUSDT spot.
    pub instruments_body: Mutex<Option<String>>,
    /// Body served by `/instruments` once at least one `/ws` upgrade has
    /// happened. Models the venue registering a symbol AT BIND, which is why a
    /// client can only learn an unconfigured symbol's shape after binding it.
    pub instruments_after_bind: Mutex<Option<String>>,
    /// Number of `POST /control/divergence` requests served.
    pub control_hits: AtomicUsize,
    /// Raw bodies of each `/control/divergence` POST (for round-trip asserts).
    pub control_bodies: Mutex<Vec<String>>,
    /// Trade frames the WS leg pushes after a client `Subscribe`.
    pub ws_trades: Mutex<Vec<String>>,
    /// Optional body served by `/quotes`.
    pub quotes_body: Mutex<Option<String>>,
    /// The `/quotes` twin of [`StubState::trades_tape`]: a sorted quote tape
    /// served with the real inclusive-`start` cursor semantics.
    pub quotes_tape: Mutex<Option<Vec<mogwai_protocol::QuoteTick>>>,
    /// The `start` query value of each `/quotes` request, in arrival order.
    pub quotes_starts: Mutex<Vec<Option<u64>>>,
    /// Execution frames the WS leg pushes after a client `SubmitOrder`.
    pub ws_exec_frames: Mutex<Vec<String>>,
    /// Execution frames the WS leg pushes after a client `ModifyOrder`. The
    /// amend leg is separate from `ws_exec_frames` because the same socket
    /// carries both and a test that seeds an amend ack must not have it
    /// delivered on the submit.
    pub ws_modify_frames: Mutex<Vec<String>>,
    /// Raw text of every `ClientMessage` frame the stub received, in arrival
    /// order. A test asserting that a field CROSSED THE WIRE (rather than
    /// merely being accepted by the client) reads it here.
    pub ws_client_messages: Mutex<Vec<String>>,
    /// WS upgrade attempts (handshakes the stub started serving). The
    /// idle-reconnect and max-attempts tests count (re)connections with this.
    pub ws_handshakes: AtomicUsize,
    /// WS `Ping` frames received from the client (heartbeat probes).
    pub ws_pings: AtomicUsize,
    /// `Pong` frames the client returned in reply to a server `Ping`.
    pub ws_pongs: AtomicUsize,
    /// `ServerMessage` JSON the stub `Ping`s the client with, after subscribe,
    /// to exercise the inbound `Ping` -> client `Pong` reply path.
    pub ws_server_pings: AtomicUsize,
    /// JSON body of each HTTP `GET /trades` response. Defaults to `[]`.
    pub trades_body: Mutex<Option<String>>,
    /// Optional successive `/trades` bodies for pagination tests.
    pub trades_pages: Mutex<VecDeque<String>>,
    /// A `ts_event`-sorted trade tape served with REAL cursor semantics: the
    /// handler honours the inclusive `start` bound and the `limit`, exactly as
    /// `GET /trades` does. `trades_pages` cannot detect a lost row, because it
    /// replays queued bodies whatever the client asked for; a cursor that skips
    /// a timestamp group is only observable against a tape.
    pub trades_tape: Mutex<Option<Vec<mogwai_protocol::TradeTick>>>,
    /// The `start` query value of each `/trades` request, in arrival order.
    pub trades_starts: Mutex<Vec<Option<u64>>>,
    /// When true, `GET /trades` answers `500`, modelling a venue that refuses
    /// the history fetch. The request generators must still emit a (empty)
    /// response rather than leaving the nautilus request unresolved.
    pub fail_trades: AtomicBool,
    /// JSON body of `GET /clock`. Unset falls through to the catch-all `[]`,
    /// which the client cannot decode and so treats as an identity clock with
    /// an UNKNOWN tape floor (`data_origin_ns` 0) - fine for most tests, but it
    /// disables the adapter's off-tape window guard, so any test exercising
    /// that guard must publish a real envelope here.
    pub clock_body: Mutex<Option<String>>,
    /// When true, `GET /account` returns an empty account snapshot. Defaults to
    /// false so older-server compatibility remains the default stub behavior.
    pub serve_account: AtomicBool,
    /// Body served for `GET /account` when `serve_account` is set.
    pub account_body: Mutex<Option<String>>,
    /// The run this stub reports on `GET /health`. A client configured with a
    /// different `expected_run_seed` must refuse to use the connection.
    pub run_seed: AtomicU64,
    /// When true, `GET /health` answers `500` with an empty body, modelling a
    /// venue whose identity probe CANNOT BE ANSWERED - the shape the adapter
    /// classifies as `IdentityOutcome::Unreachable` and deliberately declines to
    /// refuse on. Without this the stub can only model a venue that answers, so
    /// the unreachable branch has no end-to-end fixture at all.
    pub fail_health: AtomicBool,
    /// Number of `GET /health` requests served. An identity test concluding
    /// "the client did not refuse" must first establish that it ASKED: a client
    /// that skipped the probe entirely satisfies the same assertion for free.
    pub health_hits: AtomicUsize,
    /// Wall instant at which the WS leg put its FIRST `ws_trades` frame on the
    /// wire. A test measuring an inbound-latency contribution starts its clock
    /// here rather than at `connect()`: everything the stub does between the
    /// upgrade and the push - scheduling turns, blackout windows, whatever the
    /// harness grows next - is otherwise counted as client-side latency, and a
    /// delay the client never applied passes the assertion.
    pub ws_first_frame_at: Mutex<Option<Instant>>,
    /// `ws_first_frame_at`'s twin for the EXEC leg: the wall instant at which
    /// the stub was ABOUT TO SEND its first `ws_exec_frames` frame in reply to a
    /// `SubmitOrder`. Stamped strictly before the send, exactly as the data leg
    /// is, so everything above the stamp is stub time and the measured interval
    /// is `>=` the client's own contribution rather than `==` it - the phrasing
    /// matters because an earlier anchor makes a `>=` lower bound very slightly
    /// WEAKER, never stricter.
    ///
    /// SINGLE-SHOT ACROSS THE WHOLE `StubState`, like its data-leg twin: it
    /// records the first exec frame of the FIRST socket and never moves again.
    /// A test that submits two orders, or that lets the exec leg reconnect,
    /// would silently measure from the first submit - which is the "the anchor
    /// is not what it reads as" defect this field was added to remove. Such a
    /// test owes a per-submit anchor, not this one.
    ///
    /// It exists for the same reason and against a measured instance of the same
    /// defect. An exec-latency test that measures from before `connect()` is
    /// charging the client for the connect ladder, and on this leg that ladder
    /// is not small: `await_account_registered` blocks connect until the seeded
    /// account snapshot has come back through the very latency pump under test,
    /// so an armed 400 ms exec delay is paid ONCE INSIDE CONNECT before the
    /// order is even submitted. Setup measured 416.7-418.7 ms over 40 runs of
    /// `havoc_reaches_the_order_a_trigger_produces`, whose lower bound was
    /// 400 ms: satisfied by setup alone, in every run, whatever the client did
    /// with the trigger.
    pub ws_first_exec_frame_at: Mutex<Option<Instant>>,
    /// Venue-truth order rows returned to reconciliation queries.
    pub venue_orders: Mutex<Vec<OrderStatusInfo>>,
    /// Venue-truth fill rows returned to reconciliation queries.
    pub venue_fills: Mutex<Vec<OrderFilled>>,
    /// Number of venue-truth order queries served over either transport.
    pub order_queries: AtomicUsize,
    /// Number of venue-truth fill queries served over either transport.
    pub fill_queries: AtomicUsize,
    /// Timestamps of each HTTP request the stub served. The quota tests assert
    /// the gaps between consecutive entries. Today every entry is a `/trades`
    /// fetch: order entry is websocket-only, so there is no HTTP order carrier
    /// left to time.
    pub http_request_times: Mutex<Vec<Instant>>,
    /// Count of `GET /trades` requests served (polling-repeat assertion).
    pub trades_hits: AtomicUsize,
    /// Count of WS `/ws` upgrades (the polling profile must never open one).
    pub ws_hits: AtomicUsize,
    /// The request LINE of every `/ws` upgrade this stub was offered, in order.
    /// Carries the query string, and therefore the account id a client
    /// disclosed - which is what makes "what did the stranger learn" an
    /// assertable question rather than an inference from a counter.
    pub ws_requests: Mutex<Vec<String>>,
    /// Number of upgraded sockets whose handler is still alive.
    pub active_ws: AtomicUsize,
    /// When true, `serve_ws` drops the connection before completing the upgrade,
    /// modelling a venue that refuses the socket. The handshake is still counted
    /// so the attempt-cap test can pin the count.
    pub refuse_ws: AtomicBool,
    /// When set, the WS leg suppresses application frames sent within this many
    /// ms of the subscribe instant, modelling a `GoDark` blackout window.
    pub dark_ms: AtomicUsize,
    /// When true, the WS data leg closes the connection (returns, dropping the
    /// socket) right after pushing all `ws_trades` on the FIRST subscribe,
    /// modelling a clean stream end. The peer close makes the client's reader
    /// loop exit normally and run its on-disconnect flush callback - the only
    /// path that releases a message the reorder filter is holding at stream end
    /// (the flush never runs on a client-side `stop()`, which merely aborts the
    /// reader task: that is finding A.5). On any later (reconnected) subscribe
    /// the leg serves nothing and STAYS UP, so the held trade is not replayed
    /// and the client is not driven into a re-serve/close loop. Enforced by
    /// `served_once`.
    pub close_after_trades: AtomicBool,
    /// Internal: latched once a `close_after_trades` leg has served its one
    /// batch and closed. THE SENTENCE ABOVE IS THIS FIELD - without it the leg
    /// re-reads `ws_trades` on every upgrade, and since the close it just sent
    /// is exactly what makes the client re-dial, the stub spins re-serving the
    /// whole batch and closing again for as long as the test runs. Nothing
    /// asserts on it, which is why it went unnoticed: the one test using this
    /// switch stops reading after three trades and passes over a stub still
    /// looping underneath it.
    served_once: AtomicBool,
    /// Gate on the WS leg's FIRST application push. See [`PushGate`].
    pub push_gate: PushGate,
}

/// The condition the WS leg's tape push waits on, replacing a fixed sleep.
///
/// THE PROBLEM IT SOLVES. A data client's subscription is satisfied ENTIRELY
/// LOCALLY - `subscribe_trades` sends no wire frame - so a tape frame that
/// reaches the client before the local subscription is recorded is legitimately
/// discarded, and the test then fails with "expected a trade data event, got
/// Instrument" or a timeout. A WRONG ANSWER, not a clean one. The harness used
/// to buy the client a head start with an unconditional `sleep(100ms)` before
/// the push: a bet that connect-plus-subscribe fits in 100 ms of wall time,
/// which a loaded box loses, and a flat 100 ms added to every socket test that
/// seeds a frame.
///
/// The stub CANNOT observe the subscription - nothing about it crosses the
/// wire - so the test has to say when it is ready. That is what this is: the
/// test calls [`PushGate::open`] after subscribing (or, where the property is
/// about a PRE-subscription frame, before), and the leg pushes then and not
/// before.
///
/// IT LATCHES. A reconnecting client re-enters `serve_ws`, and a one-shot
/// permit would strand the second socket forever; once open, every later socket
/// pushes immediately.
///
/// ONLY THE PUSH WAITS. A leg with an empty `ws_trades` has nothing to gate, so
/// it never waits at all - which is where most of the removed wall time comes
/// from, since the exec and reconciliation binaries seed no tape.
pub struct PushGate {
    open: tokio::sync::watch::Sender<bool>,
}

impl Default for PushGate {
    fn default() -> Self {
        Self {
            open: tokio::sync::watch::channel(false).0,
        }
    }
}

/// How long the WS leg waits for a test to open the gate before giving up.
///
/// ONE BOUND, NOT TWO. Below, the legitimate wait is one `connect()` return
/// plus a synchronous `subscribe_*` - single-digit milliseconds - so a second
/// is two orders of magnitude of headroom and cannot be lost to host load.
/// Above, this is only how long a FAILING run spends before it records the
/// stall; it does not have to beat any downstream deadline, because the stall
/// is reported by [`assert_push_gate_opened`] wherever a test waits, and by
/// `StubState`'s `Drop` otherwise. An earlier draft made this constant race
/// `next_trade`'s two seconds, on the theory that the gate had to complain
/// first to be the named failure; that theory rested on a panic inside a
/// detached `tokio::spawn`, which the runtime swallows.
const PUSH_GATE_DEADLINE: Duration = Duration::from_secs(1);

thread_local! {
    /// Set when a WS leg gave up waiting for [`PushGate::open`]. THREAD-LOCAL
    /// RATHER THAN GLOBAL BECAUSE EVERY TEST IN THESE BINARIES IS
    /// `flavor = "current_thread"`: the stub's tasks are spawned onto the
    /// runtime the test itself blocks on, so they run on the test's own thread
    /// and a flag set there is readable by the test and by nothing else. The
    /// authoritative failure is `StubState`'s `Drop`, which reads the same flag
    /// on the same thread during runtime shutdown; the explicit checks exist
    /// only so the gate is named BEFORE a downstream timeout gets to speak.
    static PUSH_GATE_STALLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Panics naming the gate if any WS leg on this thread gave up waiting for it.
///
/// CALL THIS WHEREVER A TEST WAITS FOR SOMETHING THE GATED PUSH PRODUCES,
/// before that wait reports its own timeout. Without it the symptom is a
/// missing data event, which is exactly the wrong answer the gate was built to
/// replace.
///
/// NOTHING DETECTS A WAIT SITE THAT FORGOT THIS, and a `Drop` backstop on
/// `StubState` was built to be that detector and then MEASURED NOT TO WORK: the
/// state is released when the runtime drops the handler task holding it, tokio
/// catches panics in task destructors, and the bite-check printed the panic
/// under a test libtest still reported as passing. That is the same
/// swallowed-panic defect this whole change is closing, so it was removed
/// rather than kept as a verdict nobody delivers. The call sites are the
/// mechanism; a new one that waits on a tape frame owes itself this line.
pub fn assert_push_gate_opened() {
    assert!(
        !PUSH_GATE_STALLED.with(std::cell::Cell::get),
        "the stub had tape frames to push and no test opened `push_gate` \
         within {PUSH_GATE_DEADLINE:?}, so nothing was ever sent; a test that \
         seeds `ws_trades` must call `state.push_gate.open()` once its \
         subscription is recorded"
    );
}

impl PushGate {
    /// Releases the tape push, now and for every later socket.
    pub fn open(&self) {
        self.open.send_replace(true);
    }

    /// Waits for [`PushGate::open`], returning whether it opened.
    ///
    /// A TIMEOUT RECORDS AND RETURNS rather than panicking. It used to assert
    /// here, which cannot fail the test: this runs inside the per-connection
    /// `tokio::spawn` in `run_stub` whose `JoinHandle` is dropped, so the
    /// runtime captures the panic, prints it and moves on - and the unwind
    /// dropped the socket, which the client read as a dead venue and re-dialled
    /// into a fresh leg that waited and panicked again. A forgotten `open` was
    /// therefore a reconnect storm ending in a downstream timeout, which is the
    /// wrong answer this whole mechanism exists to remove.
    pub async fn wait(&self) -> bool {
        let mut rx = self.open.subscribe();
        let wait = async {
            // `borrow_and_update` marks the value seen BEFORE awaiting the next
            // change, so an `open` racing this loop is observed rather than
            // slept through.
            while !*rx.borrow_and_update() {
                if rx.changed().await.is_err() {
                    return;
                }
            }
        };
        if tokio::time::timeout(PUSH_GATE_DEADLINE, wait).await.is_ok() {
            return true;
        }
        PUSH_GATE_STALLED.with(|stalled| stalled.set(true));
        false
    }
}

/// Binds an ephemeral loopback listener, spawns the stub against it, and returns
/// the `ws://` base URL the client config consumes.
pub async fn bound_stub(state: Arc<StubState>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind stub");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(run_stub(listener, state));
    format!("ws://127.0.0.1:{port}")
}

/// Accepts connections forever, dispatching each to [`handle_connection`].
pub async fn run_stub(listener: TcpListener, state: Arc<StubState>) {
    loop {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            handle_connection(&mut stream, state).await;
        });
    }
}

async fn handle_connection(stream: &mut TcpStream, state: Arc<StubState>) {
    let Some((head, body)) = read_request(stream).await else {
        return;
    };
    let path = head.split_whitespace().nth(1).unwrap_or("/");

    if path.starts_with("/ws") {
        serve_ws(stream, head, state).await;
    } else if path.starts_with("/account") {
        if state.serve_account.load(Ordering::Relaxed) {
            let body = state
                .account_body
                .lock()
                .expect("account body mutex")
                .clone()
                .unwrap_or_else(|| account_json("MOGWAI-001", "[]", 0));
            respond_json(stream, "200 OK", &body).await;
        } else {
            respond_json(stream, "404 Not Found", "").await;
        }
    } else if path.starts_with("/health") {
        state.health_hits.fetch_add(1, Ordering::Relaxed);
        if state.fail_health.load(Ordering::Relaxed) {
            // Answered, but with nothing usable: the probe was made and could
            // not be resolved, which is the `Unreachable` shape rather than a
            // mismatch.
            respond_json(stream, "500 Internal Server Error", "").await;
            return;
        }
        // The run this stub claims to be. A client bound to a different run must
        // refuse it - that is the whole of the venue-identity check.
        let run_seed = state.run_seed.load(Ordering::Relaxed);
        respond_json(
            stream,
            "200 OK",
            &format!(r#"{{"status":"ok","oms_type":"netting","run_seed":{run_seed}}}"#),
        )
        .await;
    } else if path.starts_with("/clock") {
        let body = state.clock_body.lock().expect("clock body mutex").clone();
        match body {
            Some(body) => respond_json(stream, "200 OK", &body).await,
            None => respond_json(stream, "200 OK", "[]").await,
        }
    } else if path.starts_with("/instruments") {
        // The real venue REGISTERS a symbol when a socket binds it, so its
        // `/instruments` answer grows across a bind. Model that rather than a
        // fixed body, or a client's post-bind reseed reads the same list twice
        // and the test cannot tell a barrier from a no-op.
        let after_bind = state
            .instruments_after_bind
            .lock()
            .expect("post-bind instruments mutex")
            .clone()
            .filter(|_| state.ws_hits.load(Ordering::Relaxed) > 0);
        let body = after_bind.unwrap_or_else(|| {
            state
                .instruments_body
                .lock()
                .expect("instruments body mutex")
                .clone()
                .unwrap_or_else(|| INSTRUMENTS_JSON.to_string())
        });
        respond_json(stream, "200 OK", &body).await;
    } else if path.starts_with("/trades") {
        state.trades_hits.fetch_add(1, Ordering::Relaxed);
        state
            .http_request_times
            .lock()
            .expect("http request times mutex")
            .push(Instant::now());
        let start = query_value(path, "start").and_then(|value| value.parse::<u64>().ok());
        state
            .trades_starts
            .lock()
            .expect("trades starts mutex")
            .push(start);
        let tape = state.trades_tape.lock().expect("trades tape mutex").clone();
        if state.fail_trades.load(Ordering::Relaxed) {
            respond_json(stream, "500 Internal Server Error", "").await;
        } else if let Some(tape) = tape {
            let limit = query_value(path, "limit")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(usize::MAX);
            let end = query_value(path, "end").and_then(|value| value.parse::<u64>().ok());
            let rows = tape
                .into_iter()
                .filter(|trade| start.is_none_or(|start| trade.ts_event >= start))
                .filter(|trade| end.is_none_or(|end| trade.ts_event <= end))
                .take(limit)
                .collect::<Vec<_>>();
            respond_json(
                stream,
                "200 OK",
                &serde_json::to_string(&rows).expect("tape page json"),
            )
            .await;
        } else {
            let body = state
                .trades_pages
                .lock()
                .expect("trades pages mutex")
                .pop_front()
                .or_else(|| state.trades_body.lock().expect("trades body mutex").clone())
                .unwrap_or_else(|| "[]".to_string());
            respond_json(stream, "200 OK", &body).await;
        }
    } else if path.starts_with("/quotes") {
        let start = query_value(path, "start").and_then(|value| value.parse::<u64>().ok());
        state
            .quotes_starts
            .lock()
            .expect("quotes starts mutex")
            .push(start);
        let tape = state.quotes_tape.lock().expect("quotes tape mutex").clone();
        if let Some(tape) = tape {
            let limit = query_value(path, "limit")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(usize::MAX);
            let end = query_value(path, "end").and_then(|value| value.parse::<u64>().ok());
            let rows = tape
                .into_iter()
                .filter(|quote| start.is_none_or(|start| quote.ts_event >= start))
                .filter(|quote| end.is_none_or(|end| quote.ts_event <= end))
                .take(limit)
                .collect::<Vec<_>>();
            respond_json(
                stream,
                "200 OK",
                &serde_json::to_string(&rows).expect("quote tape page json"),
            )
            .await;
        } else {
            let body = state
                .quotes_body
                .lock()
                .expect("quotes body mutex")
                .clone()
                .unwrap_or_else(|| "[]".to_string());
            respond_json(stream, "200 OK", &body).await;
        }
    } else if path.starts_with("/control/divergence") {
        state.control_hits.fetch_add(1, Ordering::Relaxed);
        state
            .control_bodies
            .lock()
            .expect("control bodies mutex")
            .push(String::from_utf8_lossy(&body).to_string());
        respond_json(stream, "202 Accepted", "").await;
    } else {
        respond_json(stream, "200 OK", "[]").await;
    }
}

/// The largest request head this stub will accumulate before giving up. A peer
/// that never sends `\r\n\r\n` must not be able to grow the buffer without
/// bound; nothing the adapter sends comes near this.
const MAX_HEAD_BYTES: usize = 64 * 1024;

/// Reads one HTTP request, honouring `Content-Length` so POST bodies (the
/// `/control/divergence` divergence payloads) are read in full rather than
/// truncated at the header boundary.
///
/// BOTH READS LOOP. The body loop below is the obvious one; the HEAD loop is
/// the one that was missing, and its absence was invisible rather than benign.
/// A single `read` into a fixed 4 KiB buffer is not a request - it is one
/// SEGMENT of one - so a `/ws` upgrade split across two TCP segments, or any
/// head over 4 KiB (which one `Authorization` header or a cookie jar would
/// produce), found no `\r\n\r\n`, returned `None`, and dropped the connection
/// with no diagnostic at all. The client then reports a connect failure and the
/// test blames the adapter. On loopback with today's small heads it practically
/// always fit in one segment, which is exactly why it survived: the defect is
/// latent in the harness, not in what it is testing.
pub async fn read_request(stream: &mut TcpStream) -> Option<(String, Vec<u8>)> {
    let mut buf = vec![0u8; 4096];
    let mut bytes = Vec::new();
    let header_end = loop {
        // Re-scanned from the start each pass, deliberately: the terminator can
        // straddle a segment boundary, so a scan of only the newest bytes would
        // miss it. Heads are small enough that the rescan is free.
        if let Some(end) = find_header_end(&bytes) {
            break end;
        }
        // The cap bounds what is ACCUMULATED, so the read is clamped to the
        // remaining room rather than checked after the fact. Checking a
        // whole-buffer read afterwards makes the true bound
        // `MAX_HEAD_BYTES + 4096` and fires one pass late, which is the sort of
        // off-by-one-read that turns a stated exact bound into prose.
        let room = MAX_HEAD_BYTES - bytes.len();
        if room == 0 {
            return None;
        }
        let take = room.min(buf.len());
        let n = stream.read(&mut buf[..take]).await.ok()?;
        if n == 0 {
            return None;
        }
        bytes.extend_from_slice(&buf[..n]);
    };
    let head = String::from_utf8_lossy(&bytes[..header_end]).to_string();
    let content_length = content_length(&head);
    let body_start = header_end + 4;
    while bytes.len().saturating_sub(body_start) < content_length {
        let n = stream.read(&mut buf).await.ok()?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buf[..n]);
    }
    let body_end = body_start.saturating_add(content_length).min(bytes.len());
    Some((head, bytes[body_start..body_end].to_vec()))
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

/// Extracts the `Content-Length` header value, defaulting to `0`.
pub fn content_length(head: &str) -> usize {
    head.lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim())
        })
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0)
}

/// Reads one query-string parameter out of a request target. The adapter
/// percent-encodes nothing in the history query (symbol, start, end and limit
/// are all alphanumeric), so a plain split is sufficient and keeps the stub
/// free of a URL dependency.
pub fn query_value(path: &str, key: &str) -> Option<String> {
    path.split_once('?')?
        .1
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(name, _)| *name == key)
        .map(|(_, value)| value.to_string())
}

/// Writes a one-shot `Connection: close` JSON response with the given status.
pub async fn respond_json(stream: &mut TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    drop(stream.write_all(response.as_bytes()).await);
    drop(stream.flush().await);
}

/// Completes a tungstenite server handshake against an already-read request
/// head, then pushes the state-driven frames. Data clients send `Subscribe` and
/// receive `ws_trades`; exec clients send `SubmitOrder` and receive
/// `ws_exec_frames`. Matching is on a parsed `ClientMessage`, not a substring.
pub async fn serve_ws(stream: &mut TcpStream, head: String, state: Arc<StubState>) {
    use tokio_tungstenite::tungstenite::handshake::derive_accept_key;

    state.ws_handshakes.fetch_add(1, Ordering::Relaxed);
    state.ws_hits.fetch_add(1, Ordering::Relaxed);

    // The REQUEST LINE, recorded before anything is served. A stub that only
    // counts upgrades cannot answer what a client DISCLOSED to whoever was
    // holding the port - the account id rides this query string - so counting
    // alone would leave the port-reuse question unanswerable from this side.
    if let Some(request_line) = head.lines().next() {
        state
            .ws_requests
            .lock()
            .expect("ws request mutex")
            .push(request_line.to_owned());
    }

    // Model a venue that refuses the socket: the TCP dial succeeded (counted
    // above) but the WebSocket upgrade never completes, so the client treats the
    // dial as failed and backs off into its reconnect loop.
    if state.refuse_ws.load(Ordering::Relaxed) {
        return;
    }

    let key = head
        .lines()
        .find_map(|line| line.strip_prefix("Sec-WebSocket-Key: "))
        .map(str::trim)
        .unwrap_or_default();
    let accept = derive_accept_key(key.as_bytes());
    let upgrade = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
    );
    if stream.write_all(upgrade.as_bytes()).await.is_err() {
        return;
    }
    drop(stream.flush().await);

    struct ActiveWs(Arc<StubState>);
    impl Drop for ActiveWs {
        fn drop(&mut self) {
            self.0.active_ws.fetch_sub(1, Ordering::Relaxed);
        }
    }
    state.active_ws.fetch_add(1, Ordering::Relaxed);
    let _active = ActiveWs(Arc::clone(&state));

    let mut ws = tokio_tungstenite::WebSocketStream::from_raw_socket(
        stream,
        tokio_tungstenite::tungstenite::protocol::Role::Server,
        None,
    )
    .await;

    use futures_util::{SinkExt, StreamExt};
    // The venue streams the one run-wide tape without a subscribe frame, so the
    // test - not a fixed sleep - says when the push may happen. See `PushGate`.
    // A leg with nothing to push never waits.
    // A `close_after_trades` leg serves its one batch on the FIRST upgrade only.
    // The close it sends is what makes the client re-dial, so re-reading
    // `ws_trades` on the second upgrade is a re-serve/close loop rather than a
    // second scenario: the replay leg pushes nothing and stays up.
    let close_after = state.close_after_trades.load(Ordering::Relaxed);
    let is_replay = close_after && state.served_once.swap(true, Ordering::Relaxed);
    let mut frames = if is_replay {
        Vec::new()
    } else {
        state.ws_trades.lock().expect("ws trades mutex").clone()
    };
    if !frames.is_empty() && !state.push_gate.wait().await {
        // The gate never opened. Push NOTHING - pushing anyway would restore
        // the raciness the gate replaced - but keep the socket alive and fall
        // through to the read loop, so the client is not driven into a
        // reconnect storm on top of the omission. The stall is recorded; see
        // `assert_push_gate_opened`.
        frames.clear();
    }
    let dark = state.dark_ms.load(Ordering::Relaxed);
    if dark > 0 {
        tokio::time::sleep(Duration::from_millis(dark as u64)).await;
    }
    for trade in frames {
        // Stamped BEFORE the send, and only for the first frame of the first
        // socket: this is the instant a latency measurement is honest from.
        // Everything above this line is stub time, not client time.
        state
            .ws_first_frame_at
            .lock()
            .expect("ws first frame instant mutex")
            .get_or_insert_with(Instant::now);
        drop(ws.send(Message::Text(trade.into())).await);
    }
    if state.ws_server_pings.load(Ordering::Relaxed) > 0 {
        drop(ws.send(Message::Ping(Vec::new().into())).await);
    }
    if close_after && !is_replay {
        drop(ws.close(None).await);
        return;
    }
    while let Some(Ok(msg)) = ws.next().await {
        match msg {
            Message::Ping(_) => {
                state.ws_pings.fetch_add(1, Ordering::Relaxed);
            }
            Message::Pong(_) => {
                state.ws_pongs.fetch_add(1, Ordering::Relaxed);
            }
            Message::Text(text) => {
                state
                    .ws_client_messages
                    .lock()
                    .expect("ws client messages mutex")
                    .push(text.to_string());
                if let Ok(message) = serde_json::from_str::<ClientMessage>(&text) {
                    serve_exec_message(&mut ws, &message, &state).await;
                }
            }
            _ => {}
        }
    }
}

/// The EXEC leg's whole behaviour: a reply to one client command.
///
/// WHY THIS IS A SEPARATE FUNCTION. The two legs share the handshake and
/// nothing else. The data leg is entirely ABOVE the read loop in `serve_ws` -
/// it pushes the tape at upgrade and then only counts control frames, because
/// a data client's subscription never crosses the wire and it sends no
/// `ClientMessage` at all (only `ExecWsCommand`s become frames, in
/// `client/exec.rs`). So every `Message::Text` a stub socket receives is exec
/// traffic by construction, and keeping the reply logic inline invited the
/// defect that was actually found here: the `ModifyOrder` arm had grown a copy
/// of the DATA leg's `close_after_trades` / `dark_ms` / server-ping / close
/// tail, written as though the read loop were the data path.
///
/// That block was unreachable three ways over and is deleted. Two of them are
/// structural rather than a fact about today's tests, which is why deleting it
/// is safe: `close_after_trades` returns from `serve_ws` before the read loop
/// is ever entered, so its guard here could not run under any fixture; and no
/// data client sends a `ClientMessage`, so no socket that has `dark_ms` or
/// `ws_server_pings` armed for the data path reaches this code by any route a
/// client can take. The `served_once` guard the block held was structurally
/// unreachable HERE and moved to the data leg above, where the behaviour its
/// field documents actually lives; the `dark_ms`, server-ping and close tails
/// were duplicates of code already running there and are simply gone.
///
/// The stub CANNOT dispatch the two legs at the upgrade, and that is a fact
/// about the adapter rather than a shortcut taken here: `MogwaiDataClientConfig`
/// and `MogwaiExecClientConfig` build the same `/ws?account=...&session=...`
/// URL (`config.rs`), so the request line does not distinguish them. The split
/// is therefore by WHERE the behaviour sits - before the loop, or in here - not
/// by two handlers chosen at the handshake.
async fn serve_exec_message<S>(
    ws: &mut tokio_tungstenite::WebSocketStream<S>,
    message: &ClientMessage,
    state: &StubState,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use futures_util::SinkExt;

    match message {
        ClientMessage::ModifyOrder { .. } => {
            let frames = state
                .ws_modify_frames
                .lock()
                .expect("ws modify frames mutex")
                .clone();
            for frame in frames {
                drop(ws.send(Message::Text(frame.into())).await);
            }
        }
        ClientMessage::SubmitOrder(_) => {
            let frames = state
                .ws_exec_frames
                .lock()
                .expect("ws exec frames mutex")
                .clone();
            for frame in frames {
                // Stamped BEFORE the send, and only for the first frame of the
                // first socket, exactly as `ws_first_frame_at` is on the data
                // leg. Everything above this line is stub time, not client
                // time. See its doc comment.
                state
                    .ws_first_exec_frame_at
                    .lock()
                    .expect("ws first exec frame instant mutex")
                    .get_or_insert_with(Instant::now);
                drop(ws.send(Message::Text(frame.into())).await);
            }
        }
        ClientMessage::QueryOrders { .. } | ClientMessage::QueryFills { .. } => {
            if let Some(reply) = venue_query_reply(message, state) {
                let json = serde_json::to_string(&reply).expect("encode venue query reply");
                drop(ws.send(Message::Text(json.into())).await);
            }
        }
        _ => {}
    }
}

/// Builds the canonical `GET /account` body, including the required account id.
pub fn account_json(account_id: &str, positions: &str, ts_event: u64) -> String {
    format!(
        r#"{{"account_id":"{account_id}","balances":[{{"currency":"USDT","total":"10000","free":"10000","locked":"0"}}],"positions":{positions},"ts_event":{ts_event}}}"#
    )
}

/// One venue-truth BTCUSDT position row for an account snapshot.
pub fn position_json(quantity: &str, avg_px: &str) -> String {
    format!(r#"{{"symbol":"BTCUSDT","quantity":"{quantity}","avg_px":"{avg_px}"}}"#)
}

/// Answers a venue-truth query using rows seeded into the shared stub.
pub fn venue_query_reply(message: &ClientMessage, state: &StubState) -> Option<ServerMessage> {
    match message {
        ClientMessage::QueryOrders {
            request_id,
            client_order_id,
            open_only,
        } => {
            state.order_queries.fetch_add(1, Ordering::Relaxed);
            let orders = state
                .venue_orders
                .lock()
                .expect("venue orders mutex")
                .iter()
                .filter(|info| {
                    client_order_id
                        .as_ref()
                        .is_some_and(|id| info.client_order_id == *id)
                        || (client_order_id.is_none() && (!open_only || info.status.is_open()))
                })
                .cloned()
                .collect();
            Some(ServerMessage::OrderStatusSnapshot(OrderStatusSnapshot {
                request_id: request_id.clone(),
                orders,
                ts_event: VENUE_SNAPSHOT_TS_EVENT,
            }))
        }
        ClientMessage::QueryFills {
            request_id,
            client_order_id,
        } => {
            state.fill_queries.fetch_add(1, Ordering::Relaxed);
            let fills = state
                .venue_fills
                .lock()
                .expect("venue fills mutex")
                .iter()
                .filter(|fill| {
                    client_order_id
                        .as_ref()
                        .is_none_or(|id| fill.client_order_id == *id)
                })
                .cloned()
                .collect();
            Some(ServerMessage::FillSnapshot(FillSnapshot {
                request_id: request_id.clone(),
                fills,
                ts_event: VENUE_SNAPSHOT_TS_EVENT,
            }))
        }
        _ => None,
    }
}

/// A BTCUSDT venue-truth order row suitable for reconciliation tests.
pub fn venue_order_row(
    client_order_id: &str,
    venue_order_id: &str,
    status: WireOrderStatus,
    filled_qty: rust_decimal::Decimal,
    ts_last: u64,
) -> OrderStatusInfo {
    OrderStatusInfo {
        client_order_id: client_order_id.to_string(),
        venue_order_id: venue_order_id.to_string(),
        symbol: mogwai_protocol::Symbol::from("BTCUSDT"),
        position_id: None,
        side: mogwai_protocol::Side::Buy,
        order_type: mogwai_protocol::OrderType::Limit,
        time_in_force: mogwai_protocol::TimeInForce::Gtc,
        status,
        quantity: rust_decimal::Decimal::from(2),
        filled_qty,
        price: Some(rust_decimal::Decimal::from(100)),
        trigger_price: None,
        ts_triggered: None,
        reduce_only: false,
        post_only: false,
        ts_accepted: 10,
        ts_last,
    }
}

/// A BTCUSDT venue-truth fill row suitable for reconciliation tests.
pub fn venue_fill_row(
    client_order_id: &str,
    venue_order_id: &str,
    trade_id: &str,
    last_qty: rust_decimal::Decimal,
    ts_event: u64,
) -> OrderFilled {
    OrderFilled {
        client_order_id: client_order_id.to_string(),
        venue_order_id: venue_order_id.to_string(),
        trade_id: trade_id.to_string(),
        symbol: mogwai_protocol::Symbol::from("BTCUSDT"),
        position_id: None,
        side: mogwai_protocol::Side::Buy,
        last_qty,
        last_px: rust_decimal::Decimal::from(100),
        leaves_qty: rust_decimal::Decimal::ONE,
        commission: rust_decimal::Decimal::ZERO,
        commission_currency: "USDT".into(),
        liquidity_side: mogwai_protocol::LiquiditySide::Taker,
        ts_event,
    }
}

/// Builds, starts and connects an execution client, registering its initial
/// account snapshot in the supplied cache before returning it. The caller
/// installs the event sink before invoking this helper.
pub async fn connected_exec_client(
    base_url: String,
    cache: Rc<RefCell<Cache>>,
    sink_rx: &mut UnboundedReceiver<ExecutionEvent>,
) -> MogwaiExecutionClient {
    let config = MogwaiExecClientConfig {
        // Stated, not defaulted: `account_id` defaults to the placeholder the
        // validator refuses, precisely so nobody can bind a socket to an
        // account they never chose. The fake venue seeds this account.
        account_id: AccountId::from("MOGWAI-001"),
        base_url,
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
        Rc::clone(&cache),
    );
    let mut client = MogwaiExecutionClient::new(core, config).expect("client builds");
    client.start().expect("start grabs sink");

    let account_id = client.account_id();
    let drain_account = async {
        match next_exec_event(sink_rx, Duration::from_secs(2)).await {
            ExecutionEvent::Account(account) => {
                assert_eq!(account.account_id, account_id);
                cache
                    .borrow_mut()
                    .add_account(account.into())
                    .expect("cache account");
            }
            other => panic!("expected initial AccountState, got {other:?}"),
        }
    };
    let (connect, ()) = tokio::join!(client.connect(), drain_account);
    connect.expect("connect seeds account");
    client
}

/// The single-symbol instrument id every test trades.
pub fn instrument_id() -> InstrumentId {
    InstrumentId::new(Symbol::from("BTCUSDT"), *MOGWAI_VENUE)
}

/// A `Trade` wire frame for the WS leg.
pub fn trade_json(ts_event: u64, price: &str) -> String {
    format!(
        r#"{{"type":"Trade","symbol":"BTCUSDT","price":"{price}","size":"1","aggressor":"Buyer","ts_event":{ts_event}}}"#
    )
}

/// Seeds a limit BTCUSDT buy order `O-1` into the cache and returns it, so the
/// exec client's submit path can look it up.
pub fn cached_order(cache: &Rc<RefCell<Cache>>) -> nautilus_model::orders::OrderAny {
    let trader_id = TraderId::from("MOGWAI-001");
    let strategy_id = StrategyId::from("S-001");
    let clock = Rc::new(RefCell::new(TestClock::new()));
    let mut factory = OrderFactory::new(trader_id, strategy_id, None, None, clock, false, false);
    let order = factory.limit(
        instrument_id(),
        OrderSide::Buy,
        Quantity::from("1"),
        Price::from("100.00"),
        Some(TimeInForce::Gtc),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some(ClientOrderId::from("O-1")),
    );
    cache
        .borrow_mut()
        .add_order(
            order.clone(),
            None,
            Some(ClientId::from("MOGWAI-EXEC")),
            false,
        )
        .expect("cache order");
    order
}

/// Seeds a BTCUSDT sell `StopMarketOrder` `O-STOP` into the cache and returns
/// it. The protective-leg shape: reduce-only, GTC, last-price trigger.
pub fn cached_stop_market(cache: &Rc<RefCell<Cache>>) -> nautilus_model::orders::OrderAny {
    let mut factory = order_factory();
    let order = factory.stop_market(
        instrument_id(),
        OrderSide::Sell,
        Quantity::from("1"),
        Price::from("95.00"),
        Some(TriggerType::LastPrice),
        Some(TimeInForce::Gtc),
        None,
        Some(true),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some(ClientOrderId::from("O-STOP")),
    );
    cache_order(cache, &order);
    order
}

/// Seeds a BTCUSDT sell `StopLimitOrder` `O-STOPLIMIT` into the cache: trigger
/// 95, limit 94, the shape whose trigger survives into a live limit and is
/// therefore the one an amend can walk backwards.
pub fn cached_stop_limit(cache: &Rc<RefCell<Cache>>) -> nautilus_model::orders::OrderAny {
    let mut factory = order_factory();
    let order = factory.stop_limit(
        instrument_id(),
        OrderSide::Sell,
        Quantity::from("1"),
        Price::from("94.00"),
        Price::from("95.00"),
        Some(TriggerType::LastPrice),
        Some(TimeInForce::Gtc),
        None,
        None,
        Some(true),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some(ClientOrderId::from("O-STOPLIMIT")),
    );
    cache_order(cache, &order);
    order
}

/// Seeds a BTCUSDT sell `TrailingStopMarketOrder` `O-TRAIL` into the cache. The
/// venue models no trailing state, so this is the shape that must still be
/// refused by name after conditionals landed.
pub fn cached_trailing_stop(cache: &Rc<RefCell<Cache>>) -> nautilus_model::orders::OrderAny {
    let mut factory = order_factory();
    let order = factory.trailing_stop_market(
        instrument_id(),
        OrderSide::Sell,
        Quantity::from("1"),
        rust_decimal::Decimal::from(1),
        Some(TrailingOffsetType::PriceTier),
        None,
        Some(Price::from("95.00")),
        Some(TriggerType::LastPrice),
        Some(TimeInForce::Gtc),
        None,
        Some(true),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some(ClientOrderId::from("O-TRAIL")),
    );
    cache_order(cache, &order);
    order
}

fn order_factory() -> OrderFactory {
    OrderFactory::new(
        TraderId::from("MOGWAI-001"),
        StrategyId::from("S-001"),
        None,
        None,
        Rc::new(RefCell::new(TestClock::new())),
        false,
        false,
    )
}

fn cache_order(cache: &Rc<RefCell<Cache>>, order: &nautilus_model::orders::OrderAny) {
    cache
        .borrow_mut()
        .add_order(
            order.clone(),
            None,
            Some(ClientId::from("MOGWAI-EXEC")),
            false,
        )
        .expect("cache order");
}

/// The `SubmitOrder` command the exec client's `submit_order` takes, built from
/// an order's own init event. `init` lets a caller hand in a MUTATED init - the
/// unsupported-shape table drives every refusal that way, because nautilus'
/// own factory refuses to build most of them.
pub fn submit_command(
    order: &nautilus_model::orders::OrderAny,
    init: nautilus_model::events::OrderInitialized,
) -> nautilus_common::messages::execution::SubmitOrder {
    nautilus_common::messages::execution::SubmitOrder::new(
        TraderId::from("MOGWAI-001"),
        Some(ClientId::from("MOGWAI-EXEC")),
        StrategyId::from("S-001"),
        instrument_id(),
        nautilus_model::orders::Order::client_order_id(order),
        init,
        None,
        None,
        None,
        nautilus_core::UUID4::new(),
        nautilus_core::UnixNanos::default(),
        None,
    )
}

/// A BTCUSDT venue-truth row for a CONDITIONAL order, carrying the four fields
/// the conditional surface added. Reconciliation of a stop is unreadable
/// without them, which is what the report gate pins.
#[expect(clippy::too_many_arguments)]
pub fn venue_stop_order_row(
    client_order_id: &str,
    venue_order_id: &str,
    order_type: mogwai_protocol::OrderType,
    status: WireOrderStatus,
    price: Option<rust_decimal::Decimal>,
    trigger_price: rust_decimal::Decimal,
    ts_triggered: Option<u64>,
    ts_last: u64,
) -> OrderStatusInfo {
    OrderStatusInfo {
        client_order_id: client_order_id.to_string(),
        venue_order_id: venue_order_id.to_string(),
        symbol: mogwai_protocol::Symbol::from("BTCUSDT"),
        position_id: None,
        side: mogwai_protocol::Side::Sell,
        order_type,
        time_in_force: mogwai_protocol::TimeInForce::Gtc,
        status,
        quantity: rust_decimal::Decimal::from(2),
        filled_qty: rust_decimal::Decimal::ZERO,
        price,
        trigger_price: Some(trigger_price),
        ts_triggered,
        reduce_only: true,
        post_only: false,
        ts_accepted: 10,
        ts_last,
    }
}

/// Awaits the next execution event or fails after `timeout`.
pub async fn next_exec_event(
    rx: &mut UnboundedReceiver<ExecutionEvent>,
    timeout: Duration,
) -> ExecutionEvent {
    tokio::time::timeout(timeout, rx.recv())
        .await
        .expect("execution event arrives")
        .expect("sink open")
}

/// Awaits the next data event or fails after `timeout`.
pub async fn next_data_event(
    rx: &mut UnboundedReceiver<DataEvent>,
    timeout: Duration,
) -> DataEvent {
    let Ok(event) = tokio::time::timeout(timeout, rx.recv()).await else {
        // THE GATE SPEAKS FIRST. A leg that gave up waiting for `push_gate`
        // sent nothing, and the missing data event is that omission's symptom,
        // not a finding about the client.
        assert_push_gate_opened();
        panic!("data event arrives within {timeout:?}");
    };
    event.expect("sink open")
}

/// Like [`next_data_event`], but discards leading `DataEvent::Instrument`
/// frames and returns the first event that is not one.
///
/// On connect the data client publishes its seeded instrument definitions to
/// the sink before any trade or bar (`emit_seeded_instruments`), so nautilus
/// caches the instrument first. That ordering is load-bearing, not incidental:
/// a host executor refuses a bar whose instrument is absent from the
/// cache. A transport test asserting the first TRADE must therefore skip past
/// this instrument prologue rather than mistake it for the trade.
pub async fn next_non_instrument_data_event(
    rx: &mut UnboundedReceiver<DataEvent>,
    timeout: Duration,
) -> DataEvent {
    loop {
        match next_data_event(rx, timeout).await {
            DataEvent::Instrument(_) => continue,
            other => return other,
        }
    }
}
