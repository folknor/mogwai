// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Shared test-support harness for the ignored adapter integration tests.
//!
//! All three adapter test binaries (`adapter_smoke`, `data_client_transport`,
//! `havoc`) drive the public adapter path against a self-contained HTTP +
//! WebSocket stub that speaks just enough of the mogwai protocol. The stub used
//! to be copy-pasted across the three files with subtle mismatches (only the
//! havoc copy parsed `Content-Length`, `respond_json` had two different
//! signatures, `serve_ws` was fixed-frame in two and data-driven in one). This
//! module is the single, most-capable variant; each test still drives its own
//! scenario by mutating the shared [`StubState`] - the one thing that
//! legitimately differs between tests.
//!
//! The WS leg matches on a parsed [`Command`] rather than a type-name
//! substring, so a wire-format change (a field rename, an envelope change) makes
//! the stub stop recognising the client's `Subscribe` / `SubmitOrder` and the
//! test fails loudly instead of passing against a broken protocol.
//!
//! # What is and is not shared between tests in one binary
//!
//! `replace_data_event_sender` / `replace_exec_event_sender` are never global,
//! whatever their nautilus doc comments say. Both are `thread_local!` in
//! `nautilus_common::live::runner` (`DATA_EVENT_SENDER.with(...)`), libtest runs
//! each test on its own thread, and every test in these binaries is
//! `flavor = "current_thread"` so the client's tasks run on that same thread.
//! Each test therefore owns its sink outright, which is what makes the several
//! negative assertions here (`assert_no_exec_event`, the bounded tail drains)
//! sound: a 250 ms window on a shared channel would be leaky, and these are not
//! shared. Do not consolidate them onto one sender. Nothing else in scope is
//! process-wide either - no global logger, no capture buffer, no environment
//! variable, and every listener is `127.0.0.1:0`.
//!
//! And that holds in every lane, including `--test-threads=1`. This is the one
//! part of the claim that invites a wrong model, and a cold review reached the
//! wrong one: it read `--test-threads=1` as making libtest run tests inline on
//! the main thread, which would share one `EXEC_EVENT_SENDER` across a whole
//! binary and make every negative window above leaky. It does not. libtest
//! spawns a fresh named thread per test unconditionally on any threaded
//! target, because the thread name is how a panic gets attributed to a test,
//! and `--test-threads` caps how many run at once rather than whether a thread
//! is made at all.
//! Measured, not assumed: with the serial runner in the release lane (the one
//! that passes `--test-threads=1`), a probe of `thread::current()` and the
//! runner slot reported a distinct `ThreadId` named for each test in
//! `adapter_smoke`, and an empty sender slot on entry to every one of them.
//! `owns_a_fresh_exec_sink_on_every_lane` below pins it, and
//! [`assert_owns_a_fresh_exec_sink`] lets an individual test restate the
//! premise it depends on, so a libtest change fails on the premise rather than
//! on whatever the test was really asserting.
//!
//! The process-wide value here is the callsign id.
//! `mogwai_adapter::config`'s `process_callsign` is a `OnceLock`, by design -
//! one worker process presents one identity, so its data and execution legs share that
//! identity and cannot evict each other off their shared ledger. The
//! consequence for these binaries is that every client built through a
//! `Mogwai*ClientConfig::default()` presents the same callsign string, which is
//! harmless only because each test binds its own stub and no two clients ever
//! meet on one venue. Two things follow, and both bite silently:
//!
//! - A test wanting two distinct clients on one venue must set `callsign:`
//!   explicitly (`havoc.rs` uses `callsign: None` throughout). A callsign-eviction
//!   test written the obvious way, with two defaulted configs, is asserting
//!   against a constant.
//! - A test wanting "same process, two legs, no eviction" is likewise testing a
//!   constant unless it asserts on what crossed the wire. That is what
//!   `adapter_smoke::both_legs_disclose_one_process_callsign_on_the_upgrade`
//!   does, off `ws_requests`, and it is the only socket test in the crate that
//!   fails when the default is removed. Re-measured by the close pass, because
//!   the stronger phrasing that stood here ("the only thing in the crate") is
//!   false and misleadingly so: `config`'s own
//!   `both_legs_default_to_the_same_process_callsign` fails too, and it is a unit
//!   test, so cargo stops after the lib target and the four socket binaries
//!   never run at all. Anyone repeating that bite-check has to name this test
//!   directly, or the sweep reports a failure that says nothing about the wire.

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

use mogwai_adapter::{
    DEFAULT_TRADER_ID, MOGWAI_VENUE, MogwaiExecClientConfig, MogwaiExecutionClient,
};
use mogwai_protocol::{
    Command, FillSnapshot, OrderFilled, OrderStatusInfo, OrderStatusSnapshot, VenueMessage,
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

/// Asserts the premise every negative sink assertion in these binaries rests
/// on: this test entered on a thread of its own, so the runner's
/// `EXEC_EVENT_SENDER` slot is empty until this test fills it, and a timed
/// "nothing arrived" window cannot observe another test's client.
///
/// Call it at the top of any test that installs a sender and then asserts on
/// the absence of an event, and of any test whose point is that no sender
/// exists. It is cheap and it converts a silent unsoundness into a named
/// failure.
pub fn assert_owns_a_fresh_exec_sink() {
    assert!(
        nautilus_common::live::runner::try_get_exec_event_sender().is_none(),
        "an execution event sender was already installed on this test's thread: libtest is no \
         longer giving each test a fresh thread, so every negative sink window in these binaries \
         is now leaky and every test asserting no-sender is now vacuous. Fix the isolation, do \
         not relax this assertion"
    );
}

/// Pins the isolation claim in this module's header directly, in every binary
/// that includes it and therefore in every lane. One lane runs libtest
/// multi-threaded and the other at `--test-threads=1`; the per-test thread is
/// a property of libtest itself and not of the concurrency setting, so this
/// must hold in both, and it is the only test here that says so.
#[test]
fn owns_a_fresh_exec_sink_on_every_lane() {
    assert_owns_a_fresh_exec_sink();
    assert!(
        std::thread::current().name() == Some("common::owns_a_fresh_exec_sink_on_every_lane"),
        "libtest names the per-test thread after the test; got {:?}",
        std::thread::current().name()
    );
}

/// The canonical single-instrument `/instruments` seed both ends agree on.
pub const INSTRUMENTS_JSON: &str = r#"[{"symbol":"BTCUSDT","class":{"class":"spot","base":"BTC","quote":"USDT"},"price_precision":2,"size_precision":8,"price_increment":"0.01","size_increment":"0.00000001"}]"#;

/// Stable non-zero instant stamped on venue-truth query envelopes.
pub const VENUE_SNAPSHOT_TS_EVENT: u64 = 1_000_000_000;

/// The default `GET /clock` envelope: an identity simulated clock at speed 1,
/// with the tape floor at zero.
///
/// Identity is the point. `sim_epoch_ns == wall_anchor_ns == 0` with
/// `speed == 1.0` maps simulated onto wall exactly, which is what the adapter
/// falls back to when it cannot read a clock at all - so serving this changes
/// no scaling, no `ts_init`, no havoc sleep and no backoff anywhere. What it
/// changes is that the client succeeds at reading it, which is the whole
/// saving. `data_origin_ns` of zero is a known floor rather than an unknown
/// one, and no request start can precede it, so the off-river guard admits
/// exactly what it admitted before.
pub const IDENTITY_CLOCK_JSON: &str = r#"{"sim":{"sim_epoch_ns":0,"wall_anchor_ns":0,"speed":1.0},"venue_now_ns":0,"data_origin_ns":0,"warmup_ns":0}"#;

/// Test scenario state shared between the stub and the test body. A test mutates
/// the relevant fields before connecting, then reads the counters/recorded
/// bodies afterwards. Defaults model a clean, honest venue.
///
/// The axis that matters here is data-leg versus exec-leg, not HTTP versus WS,
/// and it is written down because getting it wrong is how the dead block in
/// `serve_exec_message`'s doc comment survived. Splitting this struct by
/// transport would have put `ws_trades`, `dark_ms`, `close_after_trades`,
/// `ws_venue_pings`, `ws_exec_frames` and `ws_modify_frames` in one bucket
/// together - which is precisely the confusion that produced the defect, so
/// that split localizes nothing. The ownership is:
///
/// - Data leg, all of it served before the read loop in `serve_ws`:
///   `ws_trades`, `push_gate`, `dark_ms`, `close_after_trades`,
///   `ws_venue_pings`, `ws_first_frame_at`.
/// - Exec leg, all of it served inside `serve_exec_message`: `ws_exec_frames`,
///   `ws_modify_frames`, `venue_orders`, `venue_fills`, `order_queries`,
///   `fill_queries`, `ws_first_exec_frame_at`.
/// - Either leg: the handshake and socket bookkeeping (`ws_handshakes`,
///   `ws_hits`, `ws_requests`, `active_ws`, `refuse_ws`, `ws_pings`,
///   `ws_pongs`, `ws_client_messages`) and everything HTTP.
///
/// A new field belongs in exactly one of those three, and a fixture that arms a
/// data-leg switch on a test whose client is an exec client is arming nothing:
/// exec clients never enter the tape push, data clients never send a
/// `Command`.
#[derive(Default)]
pub struct StubState {
    /// Optional body served by `/instruments`; absent uses BTCUSDT spot.
    pub instruments_body: Mutex<Option<String>>,
    /// Body served by `/instruments` once at least one `/ws` upgrade has
    /// happened. Models the venue registering a symbol at bind, which is why a
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
    /// Raw text of every `Command` frame the stub received, in arrival
    /// order. A test asserting that a field crossed the wire (rather than
    /// merely being accepted by the client) reads it here.
    pub ws_client_messages: Mutex<Vec<String>>,
    /// WS upgrade attempts (handshakes the stub started serving). The
    /// idle-reconnect and max-attempts tests count (re)connections with this.
    pub ws_handshakes: AtomicUsize,
    /// WS `Ping` frames received from the client (heartbeat probes).
    pub ws_pings: AtomicUsize,
    /// `Pong` frames the client returned in reply to a venue `Ping`.
    pub ws_pongs: AtomicUsize,
    /// `VenueMessage` JSON the stub `Ping`s the client with, after subscribe,
    /// to exercise the inbound `Ping` -> client `Pong` reply path.
    pub ws_venue_pings: AtomicUsize,
    /// JSON body of each HTTP `GET /trades` response. Defaults to `[]`.
    pub trades_body: Mutex<Option<String>>,
    /// Optional successive `/trades` bodies for pagination tests.
    pub trades_pages: Mutex<VecDeque<String>>,
    /// A `ts_event`-sorted trade tape served with real cursor semantics: the
    /// handler honours the inclusive `start` bound and the `limit`, exactly as
    /// `GET /trades` does. `trades_pages` cannot detect a lost row, because it
    /// replays queued bodies whatever the client asked for; a cursor that skips
    /// a timestamp group is only observable against a tape.
    pub trades_tape: Mutex<Option<Vec<mogwai_protocol::TradeTick>>>,
    /// The `start` query value of each `/trades` request, in arrival order.
    pub trades_starts: Mutex<Vec<Option<u64>>>,
    /// When true, `GET /trades` answers `500`, and a socket `QueryHistory` is
    /// answered with `HistoryRejected`, modelling a venue that refuses the
    /// history read. The request generators must still emit an (empty) response
    /// rather than leaving the nautilus request unresolved.
    pub fail_trades: AtomicBool,
    /// Rows per socket history page. `0` serves the whole window in one page.
    ///
    /// Set it small to exercise pagination: the client must ask again with the
    /// continuation it was handed and splice the pages without losing or
    /// repeating a row at the seam.
    pub history_page_rows: AtomicUsize,
    /// Every `QueryHistory` this stub served, as (kind, continuation), in
    /// arrival order.
    ///
    /// Recorded rather than merely counted so a test can assert the request
    /// sequence - that the client resumed with the token it was given rather
    /// than re-asking from the start, which a row-level assertion cannot
    /// distinguish from a client that paged correctly by luck.
    pub history_requests: Mutex<Vec<(mogwai_protocol::HistoryKind, Option<String>)>>,
    /// JSON body of `GET /clock`. Unset serves [`IDENTITY_CLOCK_JSON`], a real
    /// decodable envelope at speed 1 with a zero river floor, which is what a
    /// venue that has just booted looks like. A test exercising the off-river
    /// window guard publishes an envelope with a real `data_origin_ns` here;
    /// a test exercising the undecodable path sets `fail_clock`.
    ///
    /// The default used to be the catch-all `[]`, and it was the single largest
    /// cost in this crate's test suite: the client cannot decode it, and
    /// `fetch_clock_or_identity` retries three times with a 200 ms wall sleep
    /// between attempts before falling back, inline in `connect()`. That is
    /// ~400 ms on every connecting test, and all but two tests in these
    /// binaries connect. Measured per test with
    /// `scripts/adapter_test_walls.py`: they sat in a flat 419-892 ms band with
    /// a ~420 ms floor, while the two that never connect came in at 15 ms and
    /// 23 ms. The counts and the whole accounting are in
    /// `reference/performance.md` under 2026-08-19. Serving a decodable clock is behaviourally identical
    /// downstream - `ensure_on_river` is given `Some(0)` instead of `None`, and
    /// no start can precede zero - and it is not a weaker fixture but a more
    /// honest one: the real venue answers this route.
    pub clock_body: Mutex<Option<String>>,
    /// When true, `GET /clock` answers the undecodable catch-all `[]`,
    /// modelling a venue whose clock cannot be read at all. This is the only
    /// route to `fetch_clock_or_identity`'s retry-then-identity-fallback
    /// branch, which every test in these binaries used to traverse by accident
    /// and none of them asserted anything about. Costs ~400 ms of retry
    /// ladder, so arm it only in a test whose property IS that branch.
    pub fail_clock: AtomicBool,
    /// Number of `GET /clock` requests served. The retry ladder is the only
    /// thing that distinguishes a fallback from a refusal from the outside, so
    /// a test on `fail_clock` counts the attempts rather than inferring them
    /// from how long the connect took.
    pub clock_hits: AtomicUsize,
    /// When true, `GET /account` returns an empty account snapshot. Defaults to
    /// false so older-venue compatibility remains the default stub behavior.
    pub serve_account: AtomicBool,
    /// Body served for `GET /account` when `serve_account` is set.
    pub account_body: Mutex<Option<String>>,
    /// The request line of every `GET /account` this stub served, in order.
    ///
    /// It carries the query string, and therefore the account the puller named,
    /// for the reason `ws_requests` carries the socket's: the real venue
    /// resolves accounts totally and answers an unnamed pull from the run's
    /// default ledger, so a double that only replies is blind to a client
    /// reading the wrong ledger. Recording the request makes "which account did
    /// it ask about" an assertable question instead of an inference from the
    /// body the double chose to hand back.
    pub account_requests: Mutex<Vec<String>>,
    /// The run this stub reports on `GET /health`. A client configured with a
    /// different `expected_run_seed` must refuse to use the connection.
    pub run_seed: AtomicU64,
    /// When true, `GET /health` answers `500` with an empty body, modelling a
    /// venue whose identity probe cannot be answered at all - the shape the adapter
    /// classifies as `IdentityOutcome::Unreachable` and deliberately declines to
    /// refuse on. Without this the stub can only model a venue that answers, so
    /// the unreachable branch has no end-to-end fixture at all.
    pub fail_health: AtomicBool,
    /// When set, `GET /health` reports a faulted run whose tape fault names this
    /// symbol, instead of the healthy body served by default.
    ///
    /// The real endpoint's `status` varies - `ok` while no boated river carries
    /// a tape fault, `faulted` the moment one does, with a `fault` object saying
    /// which river - and a double that can only ever say `ok` cannot express the
    /// half of the contract a poller gates on. Defaults to `None`, so every
    /// existing test still meets the healthy body it met before.
    pub health_fault_symbol: Mutex<Option<String>>,
    /// Number of `GET /health` requests served. An identity test concluding
    /// "the client did not refuse" must first establish that it asked: a client
    /// that skipped the probe entirely satisfies the same assertion for free.
    pub health_hits: AtomicUsize,
    /// Wall instant at which the WS leg put its first `ws_trades` frame on the
    /// wire. A test measuring an inbound-latency contribution starts its clock
    /// here rather than at `connect()`: everything the stub does between the
    /// upgrade and the push - scheduling turns, blackout windows, whatever the
    /// harness grows next - is otherwise counted as adapter-side latency, and a
    /// delay the client never applied passes the assertion.
    pub ws_first_frame_at: Mutex<Option<Instant>>,
    /// `ws_first_frame_at`'s twin for the exec leg: the wall instant at which
    /// the stub was about to send its first `ws_exec_frames` frame in reply to a
    /// `SubmitOrder`. Stamped strictly before the send, exactly as the data leg
    /// is, so everything above the stamp is stub time and the measured interval
    /// is `>=` the client's own contribution rather than `==` it - the phrasing
    /// matters because an earlier anchor makes a `>=` lower bound very slightly
    /// weaker, never stricter.
    ///
    /// Single-shot across the whole `StubState`, like its data-leg twin: it
    /// records the first exec frame of the first socket and never moves again.
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
    /// so an armed 400 ms exec delay is paid once, inside connect, before the
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
    /// The request line of every `/ws` upgrade this stub was offered, in order.
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
    /// How many further upgrades `serve_ws` answers with the venue's real
    /// second-cadence 400, decremented on each. Models an incumbent passenger
    /// riding this river at another speed and then leaving: once the count
    /// reaches zero the upgrade is served normally, which is the transition the
    /// contract in `docs/accounts.md` promises and the only way to test that the
    /// client is still dialling when it happens.
    pub cadence_refusals: AtomicUsize,
    /// When set, the WS leg suppresses application frames sent within this many
    /// ms of the subscribe instant, modelling a `GoDark` blackout window.
    pub dark_ms: AtomicUsize,
    /// When true, the WS data leg closes the connection (returns, dropping the
    /// socket) right after pushing all `ws_trades` on the first subscribe,
    /// modelling a clean stream end. The peer close makes the client's reader
    /// loop exit normally and run its on-disconnect flush callback - the only
    /// path that releases a message the reorder filter is holding at stream end
    /// (the flush never runs on a client-side `stop()`, which merely aborts the
    /// reader task: that is finding A.5). On any later (reconnected) subscribe
    /// the leg serves nothing and stays up, so the held trade is not replayed
    /// and the client is not driven into a re-serve/close loop. Enforced by
    /// `served_once`.
    pub close_after_trades: AtomicBool,
    /// Internal: latched once a `close_after_trades` leg has served its one
    /// batch and closed. The sentence above is itself this field - without it the leg
    /// re-reads `ws_trades` on every upgrade, and since the close it just sent
    /// is exactly what makes the client re-dial, the stub spins re-serving the
    /// whole batch and closing again for as long as the test runs. Nothing
    /// asserts on it, which is why it went unnoticed: the one test using this
    /// switch stops reading after three trades and passes over a stub still
    /// looping underneath it.
    served_once: AtomicBool,
    /// Gate on the WS leg's first application push. See [`PushGate`].
    pub push_gate: PushGate,
}

/// The condition the WS leg's tape push waits on, replacing a fixed sleep.
///
/// The problem it solves. A data client's subscription is satisfied entirely
/// locally - `subscribe_trades` sends no wire frame - so a tape frame that
/// reaches the client before the local subscription is recorded is legitimately
/// discarded, and the test then fails with "expected a trade data event, got
/// Instrument" or a timeout. A wrong answer, not a clean one. The harness used
/// to buy the client a head start with an unconditional `sleep(100ms)` before
/// the push: a bet that connect-plus-subscribe fits in 100 ms of wall time,
/// which a loaded box loses, and a flat 100 ms added to every socket test that
/// seeds a frame.
///
/// The stub cannot observe the subscription at all - nothing about it crosses the
/// wire - so the test has to say when it is ready. That is what this is: the
/// test calls [`PushGate::open`] after subscribing (or, where the property is
/// about a pre-subscription frame, before), and the leg pushes then and not
/// before.
///
/// It latches. A reconnecting client re-enters `serve_ws`, and a one-shot
/// permit would strand the second socket forever; once open, every later socket
/// pushes immediately.
///
/// Only the push waits. A leg with an empty `ws_trades` has nothing to gate, so
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
/// One bound, not two. Below, the legitimate wait is one `connect()` return
/// plus a synchronous `subscribe_*` - single-digit milliseconds - so a second
/// is two orders of magnitude of headroom and cannot be lost to host load.
/// Above, this is only how long a failing run spends before it records the
/// stall; it does not have to beat any downstream deadline, because the stall
/// is reported by [`assert_push_gate_opened`] wherever a test waits, and by
/// `StubState`'s `Drop` otherwise. An earlier draft made this constant race
/// `next_trade`'s two seconds, on the theory that the gate had to complain
/// first to be the named failure; that theory rested on a panic inside a
/// detached `tokio::spawn`, which the runtime swallows.
const PUSH_GATE_DEADLINE: Duration = Duration::from_secs(1);

thread_local! {
    /// Set when a WS leg gave up waiting for [`PushGate::open`]. Thread-local
    /// rather than global because every test in these binaries is
    /// `flavor = "current_thread"`: the stub's tasks are spawned onto the
    /// runtime the test itself blocks on, so they run on the test's own thread
    /// and a flag set there is readable by the test and by nothing else. The
    /// authoritative failure is `StubState`'s `Drop`, which reads the same flag
    /// on the same thread during runtime shutdown; the explicit checks exist
    /// only so the gate is named before a downstream timeout gets to speak.
    static PUSH_GATE_STALLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Panics naming the gate if any WS leg on this thread gave up waiting for it.
///
/// Call this wherever a test waits for something the gated push produces,
/// before that wait reports its own timeout. Without it the symptom is a
/// missing data event, which is exactly the wrong answer the gate was built to
/// replace.
///
/// Nothing detects a wait site that forgot this, and a `Drop` backstop on
/// `StubState` was built to be that detector and then measured not to work: the
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
    /// A timeout records and returns rather than panicking. It used to assert
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
            // `borrow_and_update` marks the value seen before awaiting the next
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
        // `path` is the whole request target, query string included, so the
        // prefix match already accepts `/account?account=...`. Record the
        // target before answering, including on the 404 arm: a client that
        // named the wrong account against an older venue named it just the
        // same.
        state
            .account_requests
            .lock()
            .expect("account requests mutex")
            .push(path.to_string());
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
        let fault = state
            .health_fault_symbol
            .lock()
            .expect("health fault mutex")
            .clone();
        // Status and fault are one decision on the real endpoint and cannot
        // disagree there, so they are derived together here too.
        let body = match fault {
            Some(symbol) => format!(
                r#"{{"status":"faulted","oms_type":"netting","run_seed":{run_seed},"fault":{{"symbol":"{symbol}","kind":"arrival.intensity_ceiling","clock_ns":0}}}}"#
            ),
            None => format!(r#"{{"status":"ok","oms_type":"netting","run_seed":{run_seed}}}"#),
        };
        respond_json(stream, "200 OK", &body).await;
    } else if path.starts_with("/clock") {
        state.clock_hits.fetch_add(1, Ordering::Relaxed);
        if state.fail_clock.load(Ordering::Relaxed) {
            // The old default, now opt-in: a body the client cannot decode.
            respond_json(stream, "200 OK", "[]").await;
            return;
        }
        let body = state.clock_body.lock().expect("clock body mutex").clone();
        match body {
            Some(body) => respond_json(stream, "200 OK", &body).await,
            None => respond_json(stream, "200 OK", IDENTITY_CLOCK_JSON).await,
        }
    } else if path.starts_with("/instruments") {
        // The real venue registers a symbol when a socket binds it, so its
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
        let raw = String::from_utf8_lossy(&body).to_string();
        state
            .control_bodies
            .lock()
            .expect("control bodies mutex")
            .push(raw.clone());
        let (status, answer) = divergence_answer(&raw);
        respond_json(stream, status, &answer).await;
    } else {
        respond_json(stream, "200 OK", "[]").await;
    }
}

/// What this stub answers one `POST /control/divergence` body with, mirroring
/// `mogwai_venue::http::arm_divergence` rather than what a test would find
/// convenient.
///
/// It used to answer `202 Accepted` to every request, whatever was in the body,
/// and that is the double-diverging-from-the-endpoint shape `test-doctrine.md`
/// names: a test built on it proves the adapter can encode a request, never that
/// the venue would take it. It cost this suite a live one. A round of the bug
/// loop added `CancelOpenOrderSilently` to a connect-time `HavocSpec` and the
/// coverage read as real, but that arm is an immediate book action - during
/// `connect()` nothing is resting yet, so a real venue answers `404 unknown
/// order` and the adapter's `ensure!` on the status turns it into a failed
/// connect. The blanket `202` hid a configuration that cannot work.
///
/// What is modelled, and it is the venue's own ordering: a body that is not a
/// JSON object, or that names no `kind`, is a `400`; a malformed `account` is a
/// `400`; `FaultTape` carrying an account scope is a `400`; and
/// `CancelOpenOrderSilently` is a `404`, because this stub holds no book and an
/// id that rests nowhere is exactly what the venue refuses. Everything else is
/// `202` with the venue's own accepted body.
///
/// What is not modelled, stated so the next reader does not mistake this for the
/// venue: the divergence payloads themselves are not range-validated (the venue
/// runs `validate_divergence` and answers `400`), and nothing here has a ledger,
/// so no arm has an effect to observe. A test that needs either wants a real
/// venue, which this crate cannot spawn - the venue binary lives behind
/// `mogwai-cli`'s `CARGO_BIN_EXE_mogwai`.
pub fn divergence_answer(raw: &str) -> (&'static str, String) {
    let refused = |reason: &str| serde_json::json!({ "error": reason }).to_string();
    let Ok(serde_json::Value::Object(request)) = serde_json::from_str::<serde_json::Value>(raw)
    else {
        return ("400 Bad Request", refused("divergence: malformed request"));
    };
    let Some(kind) = request.get("kind").and_then(serde_json::Value::as_str) else {
        return ("400 Bad Request", refused("divergence: missing kind"));
    };
    let account = request.get("account").and_then(serde_json::Value::as_str);
    if let Some(account) = account
        && let Err(err) = mogwai_protocol::AccountId::parse(account)
    {
        return ("400 Bad Request", refused(&format!("account: {err}")));
    }
    match kind {
        "FaultTape" if account.is_some() => (
            "400 Bad Request",
            refused("FaultTape takes down the whole venue and cannot be scoped to one account"),
        ),
        "CancelOpenOrderSilently" => ("404 Not Found", refused("unknown order")),
        _ => (
            "202 Accepted",
            serde_json::json!({ "status": "accepted", "detail": null, "evicted": null })
                .to_string(),
        ),
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
/// Both reads loop. The body loop below is the obvious one; the head loop is
/// the one that was missing, and its absence was invisible rather than benign.
/// A single `read` into a fixed 4 KiB buffer is not a request - it is one
/// segment of one - so a `/ws` upgrade split across two TCP segments, or any
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
        // The cap bounds what is accumulated, so the read is clamped to the
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

/// Completes a tungstenite venue handshake against an already-read request
/// head, then pushes the state-driven frames. Data clients send `Subscribe` and
/// receive `ws_trades`; exec clients send `SubmitOrder` and receive
/// `ws_exec_frames`. Matching is on a parsed `Command`, not a substring.
pub async fn serve_ws(stream: &mut TcpStream, head: String, state: Arc<StubState>) {
    use tokio_tungstenite::tungstenite::handshake::derive_accept_key;

    state.ws_handshakes.fetch_add(1, Ordering::Relaxed);
    state.ws_hits.fetch_add(1, Ordering::Relaxed);

    // The request line, recorded before anything is served. A stub that only
    // counts upgrades cannot answer what a client disclosed to whoever was
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

    // The venue's second-cadence refusal, spoken as the venue speaks it: a 400
    // with the marker in its body, not a dropped socket. Each refusal consumes
    // one of the armed count, so the incumbent "leaves" after that many dials.
    if state
        .cadence_refusals
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
            left.checked_sub(1)
        })
        .is_ok()
    {
        let body = format!(
            "account MOGWAI-001 is already seated on BTCUSDT at speed 1; {}",
            mogwai_protocol::control::CADENCE_CONFLICT_MARKER
        );
        let refusal = format!(
            "HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        drop(stream.write_all(refusal.as_bytes()).await);
        drop(stream.flush().await);
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
    // A `close_after_trades` leg serves its one batch on the first upgrade only.
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
        // The gate never opened. Push nothing at all - pushing anyway would restore
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
        // Stamped before the send, and only for the first frame of the first
        // socket: this is the instant a latency measurement is honest from.
        // Everything above this line is stub time, not client time.
        state
            .ws_first_frame_at
            .lock()
            .expect("ws first frame instant mutex")
            .get_or_insert_with(Instant::now);
        drop(ws.send(Message::Text(trade.into())).await);
    }
    if state.ws_venue_pings.load(Ordering::Relaxed) > 0 {
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
                if let Ok(message) = serde_json::from_str::<Command>(&text) {
                    serve_exec_message(&mut ws, &message, &state).await;
                }
            }
            _ => {}
        }
    }
}

/// The exec leg's whole behaviour: a reply to one command.
///
/// Why this is a separate function. The two legs share the handshake and
/// nothing else. The data leg is entirely above the read loop in `serve_ws` -
/// it pushes the tape at upgrade and then only counts control frames, because
/// a data client's subscription never crosses the wire and it sends no
/// `Command` at all (only `ExecWsCommand`s become frames, in
/// `client/exec.rs`). So every `Message::Text` a stub socket receives is exec
/// traffic by construction, and keeping the reply logic inline invited the
/// defect that was actually found here: the `ModifyOrder` arm had grown a copy
/// of the data leg's `close_after_trades` / `dark_ms` / venue-ping / close
/// tail, written as though the read loop were the data path.
///
/// That block was unreachable three ways over and is deleted. Two of them are
/// structural rather than a fact about today's tests, which is why deleting it
/// is safe: `close_after_trades` returns from `serve_ws` before the read loop
/// is ever entered, so its guard here could not run under any fixture; and no
/// data client sends a `Command`, so no socket that has `dark_ms` or
/// `ws_venue_pings` armed for the data path reaches this code by any route a
/// client can take. The `served_once` guard the block held was structurally
/// unreachable in this leg and moved to the data leg above, where the behaviour its
/// field documents actually lives; the `dark_ms`, venue-ping and close tails
/// were duplicates of code already running there and are simply gone.
///
/// The stub cannot dispatch the two legs at the upgrade at all, and that is a fact
/// about the adapter rather than a shortcut taken here: `MogwaiDataClientConfig`
/// and `MogwaiExecClientConfig` build the same `/ws?account=...&callsign=...`
/// URL (`config.rs`), so the request line does not distinguish them. The split
/// is therefore by where the behaviour sits - before the loop, or in here - not
/// by two handlers chosen at the handshake.
async fn serve_exec_message<S>(
    ws: &mut tokio_tungstenite::WebSocketStream<S>,
    message: &Command,
    state: &StubState,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use futures_util::SinkExt;

    match message {
        Command::ModifyOrder { .. } => {
            let frames = state
                .ws_modify_frames
                .lock()
                .expect("ws modify frames mutex")
                .clone();
            for frame in frames {
                drop(ws.send(Message::Text(frame.into())).await);
            }
        }
        Command::SubmitOrder(_) => {
            let frames = state
                .ws_exec_frames
                .lock()
                .expect("ws exec frames mutex")
                .clone();
            for frame in frames {
                // Stamped before the send, and only for the first frame of the
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
        Command::QueryOrders { .. } | Command::QueryFills { .. } => {
            if let Some(reply) = venue_query_reply(message, state) {
                let json = serde_json::to_string(&reply).expect("encode venue query reply");
                drop(ws.send(Message::Text(json.into())).await);
            }
        }
        Command::QueryHistory { .. } => {
            let reply = history_reply(message, state);
            let json = serde_json::to_string(&reply).expect("encode history reply");
            drop(ws.send(Message::Text(json.into())).await);
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

/// Answers a socket history request from the seeded tapes, with the venue's own
/// paging semantics rather than with whatever the calling test needs.
///
/// That distinction is the point of writing it this way. A double that replayed
/// queued bodies whatever the client asked for could not detect a client that
/// loses a row at a page seam, re-requests from the start, or ignores its
/// continuation - the failures paging actually has. This one honours the
/// inclusive start bound, fixes a cutoff at the first page and carries it,
/// resumes strictly after the last row delivered, and reports completion only
/// when the window is exhausted.
pub fn history_reply(message: &Command, state: &StubState) -> VenueMessage {
    let Command::QueryHistory {
        request_id,
        kind,
        start,
        end,
        continuation,
    } = message
    else {
        unreachable!("history_reply is only called for a QueryHistory")
    };
    state
        .history_requests
        .lock()
        .expect("history requests mutex")
        .push((*kind, continuation.clone()));

    if state.fail_trades.load(Ordering::Relaxed) {
        return VenueMessage::HistoryRejected {
            request_id: request_id.clone(),
            reason: "stub history refusal".to_owned(),
            retryable: true,
        };
    }

    // `stub:<cutoff>:<next_ts>` - opaque to the client, which only hands it
    // back. The shape differs from the venue's on purpose: a client that had
    // learned to parse the real one would be caught here.
    let resumed: Option<(u64, u64)> = continuation.as_ref().and_then(|token| {
        let mut parts = token.split(':');
        (parts.next()? == "stub")
            .then(|| Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?)))
            .flatten()
    });
    let rows: Vec<mogwai_protocol::HistoryRow> = match kind {
        mogwai_protocol::HistoryKind::Trades => state
            .trades_tape
            .lock()
            .expect("trades tape mutex")
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(mogwai_protocol::HistoryRow::Trade)
            .collect(),
        mogwai_protocol::HistoryKind::Quotes => state
            .quotes_tape
            .lock()
            .expect("quotes tape mutex")
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(mogwai_protocol::HistoryRow::Quote)
            .collect(),
    };
    // Fixed at the first page and carried after it, so a growing tape cannot
    // move the finish line under a paginating client.
    let cutoff = resumed.map_or_else(
        || {
            end.unwrap_or_else(|| {
                rows.last()
                    .map_or(u64::MAX, mogwai_protocol::HistoryRow::ts_event)
            })
        },
        |(cutoff, _)| cutoff,
    );
    let from = resumed.map_or(start.unwrap_or(0), |(_, next)| next);
    let window: Vec<mogwai_protocol::HistoryRow> = rows
        .into_iter()
        .filter(|row| row.ts_event() >= from && row.ts_event() <= cutoff)
        .collect();
    let page_rows = state.history_page_rows.load(Ordering::Relaxed);
    let (page, complete) = if page_rows == 0 || window.len() <= page_rows {
        (window, true)
    } else {
        let mut window = window;
        window.truncate(page_rows);
        (window, false)
    };
    let continuation = (!complete)
        .then(|| {
            page.last().map(|last| {
                // Strictly after the last row delivered, which is what stops a
                // row being served twice at the seam.
                format!("stub:{cutoff}:{}", last.ts_event().saturating_add(1))
            })
        })
        .flatten();
    VenueMessage::HistoryPage {
        request_id: request_id.clone(),
        kind: *kind,
        rows: page,
        cutoff,
        continuation,
        complete,
    }
}

/// Answers a venue-truth query using rows seeded into the shared stub.
pub fn venue_query_reply(message: &Command, state: &StubState) -> Option<VenueMessage> {
    match message {
        Command::QueryOrders {
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
            Some(VenueMessage::OrderStatusSnapshot(OrderStatusSnapshot {
                request_id: request_id.clone(),
                orders,
                ts_event: VENUE_SNAPSHOT_TS_EVENT,
            }))
        }
        Command::QueryFills {
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
            Some(VenueMessage::FillSnapshot(FillSnapshot {
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
        match next_exec_event(
            sink_rx,
            Duration::from_secs(2),
            "the initial AccountState connect() seeds from GET /account",
        )
        .await
        {
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
    let trader_id = TraderId::from(DEFAULT_TRADER_ID);
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
        TraderId::from(DEFAULT_TRADER_ID),
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
/// an order's own init event. `init` lets a caller hand in a mutated init - the
/// unsupported-shape table drives every refusal that way, because nautilus'
/// own factory refuses to build most of them.
pub fn submit_command(
    order: &nautilus_model::orders::OrderAny,
    init: nautilus_model::events::OrderInitialized,
) -> nautilus_common::messages::execution::SubmitOrder {
    nautilus_common::messages::execution::SubmitOrder::new(
        TraderId::from(DEFAULT_TRADER_ID),
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

/// A BTCUSDT venue-truth row for a conditional order, carrying the four fields
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

/// Awaits the next execution event or fails after `timeout`, naming what the
/// caller was waiting for.
///
/// `what` is neither decoration nor optional. This helper is called from
/// roughly thirty sites across three binaries, and its old message -
/// `execution event arrives: Elapsed(())`, reported at this file - named
/// neither the expectation nor the wait, so every one of those thirty timeouts
/// read identically and pointed at the harness rather than at the property. It
/// was hit live during a round-4 bite-check and cost real time to attribute.
/// libtest's panic header names the test (the thread is named for it); what
/// only the call site knows is which event in the sequence failed to arrive,
/// which is what this argument carries.
///
/// The two failures are also separated. A closed sink means the client's event
/// task died or the sender was replaced out from under this receiver, which is
/// a different defect from a client that simply never emitted - and the old
/// `expect("sink open")` reported it as an unwrap on a `None`.
pub async fn next_exec_event(
    rx: &mut UnboundedReceiver<ExecutionEvent>,
    timeout: Duration,
    what: &str,
) -> ExecutionEvent {
    match tokio::time::timeout(timeout, rx.recv()).await {
        Ok(Some(event)) => event,
        Ok(None) => panic!(
            "the execution sink closed while waiting for {what}; the client's event \
             task is gone, so nothing more can ever arrive"
        ),
        Err(_) => {
            panic!("no execution event reached the sink within {timeout:?}, waiting for {what}")
        }
    }
}

/// Awaits the next data event or fails after `timeout`.
pub async fn next_data_event(
    rx: &mut UnboundedReceiver<DataEvent>,
    timeout: Duration,
) -> DataEvent {
    let Ok(event) = tokio::time::timeout(timeout, rx.recv()).await else {
        // The gate speaks first. A leg that gave up waiting for `push_gate`
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
/// cache. A transport test asserting the first trade must therefore skip past
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
