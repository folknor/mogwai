// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The serving gates: what the venue answers over HTTP and `/ws` while a run is
//! in flight - the history ceilings and floor, the boot river's warmup span
//! servable at readiness, the subscription-free feed, the per-account ledgers
//! and their policies, the cadence and eviction rules, and the divergence control
//! plane.
//!
//! It used to say "L3-L6 gates" and that meant nothing any more, which is worth
//! recording rather than silently replacing: those layer numbers came from a
//! retired plan, and a file organized by a vanished index reads as unorganized.
//!
//! What was considered and refused, so the next reader does not re-derive it.
//! A 2026-08-18 report proposed splitting this file into a `serving_readonly.rs`
//! sharing one leaked venue per config and a `serving_owned.rs` launching one
//! each, on the premise that the binary's wall floor is 54 venue boots under
//! parallel load. The premise was measured and is false. A `fast.toml` venue -
//! process launch, bind, 300 s of warmup materialized, one HTTP round trip -
//! costs about 10 ms in the profile these tests build under, and the binary's
//! whole wall at `--test-threads=8` is 9.77 s against a single test,
//! `a_market_submit_takes_a_reading_on_both_the_priced_and_priceless_paths`, at
//! 9.63 s. The floor is that one test's deliberate flake margin - eight scored
//! attempts with a 500 ms gap, which section B of the report ruled may not be
//! trimmed for wall time - and no amount of venue sharing moves it.
//!
//! The split's own axis does not survive either. Only six tests here are
//! get-only, and two of those cannot share a venue for reasons that have nothing
//! to do with writing: `history_refuses_an_illegal_symbol_and_serves_an
//! _unconfigured_one` materializes rivers, which `/instruments` then advertises,
//! and `a_paged_tape_window_equals_the_same_window_read_in_one_query` asserts
//! that its window still fits one page, which a longer-lived venue's growing
//! tape falsifies. Sharing four venues to save 40 ms is not worth an
//! order-dependence class nothing in this tree would detect.

mod common;

use std::time::Duration;

use common::{
    account_ttl_config, band_config, fast_config, http_get, http_post_json, mnq_preset_config,
    paced_config, perpetual_config, spawn, tiny_fanout_config, two_symbols_config,
};
use futures_util::{SinkExt, StreamExt};
use mogwai_protocol::{LiquiditySide, TradeTick, VenueMessage};
use tokio_tungstenite::tungstenite::Message;

/// A config whose boot river is named only by `[instrument] preset`, with no
/// top-level `symbol` key. The harness must resolve MNQ the way `serve.rs`
/// does; reading the raw config key and defaulting to `DEFAULT_PRESET` answers
/// BTCUSDT for a venue that serves MNQ, and no other config in this tree can
/// tell the two apart.
#[test]
#[ignore = "binds a loopback listener"]
fn preset_only_config_resolves_the_default_river() {
    let venue = spawn(&["--config", &mnq_preset_config()]);
    assert_eq!(venue.symbol, "MNQ");
    let (status, body) = http_get(&venue.http_base(), "/instruments");
    assert_eq!(status, 200, "instrument list answers: {body}");
    let defs: Vec<mogwai_protocol::InstrumentDef> =
        serde_json::from_str(&body).unwrap_or_else(|err| panic!("{body} is not a list: {err}"));
    assert!(
        defs.iter().any(|def| def.symbol.as_ref() == venue.symbol),
        "the venue serves its resolved boot river: {body}"
    );
}

#[test]
#[ignore = "binds a loopback listener"]
fn history_is_served_for_a_configured_symbol_that_is_not_the_default_river() {
    let venue = spawn(&["--config", &two_symbols_config()]);
    let (status, body) = http_get(
        &venue.http_base(),
        "/operator/trades?symbol=MNQ&start=0&limit=5",
    );
    assert_eq!(status, 200, "configured cold history is served: {body}");
    assert!(body.contains("MNQ"));
}

#[test]
#[ignore = "binds a loopback listener"]
fn a_pulled_account_snapshot_is_labeled_venue_clock() {
    let venue = spawn(&["--config", &fast_config()]);
    let (status, body) = http_get(&venue.http_base(), "/account");
    assert_eq!(status, 200, "account answers: {body}");
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["clock"], "venue");
    for field in ["account_id", "balances", "positions", "ts_event"] {
        assert!(
            value.get(field).is_some(),
            "flattened field {field} is missing: {body}"
        );
    }
    assert!(
        value.get("account").is_none(),
        "account payload must remain flat: {body}"
    );
}

#[test]
#[ignore = "binds a loopback listener"]
fn instruments_reports_every_configured_shape() {
    let venue = spawn(&["--config", &two_symbols_config()]);
    let (status, body) = http_get(&venue.http_base(), "/instruments");
    assert_eq!(status, 200, "instrument list answers: {body}");
    let defs: Vec<mogwai_protocol::InstrumentDef> = serde_json::from_str(&body).unwrap();
    assert_eq!(
        defs.iter()
            .map(|def| def.symbol.as_ref())
            .collect::<Vec<_>>(),
        ["BTCUSDT", "MNQ"]
    );
}

/// Piece 13: the upgrade refuses an illegal symbol, not an unconfigured one.
/// `a_run_serves_a_symbol_nobody_configured` covers the served half.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn ws_upgrade_refuses_an_illegal_symbol_with_400() {
    let venue = spawn(&["--config", &fast_config()]);
    let error = tokio_tungstenite::connect_async(venue.ws_url_for("NOT%20A%20SYMBOL"))
        .await
        .expect_err("the HTTP upgrade must be refused");
    match error {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            assert_eq!(response.status(), 400, "no upgrade occurred: {response:?}");
        }
        other => panic!("expected an HTTP refusal before upgrade, got {other}"),
    }

    let (mut socket, response) = tokio_tungstenite::connect_async(venue.ws_url_for(&venue.symbol))
        .await
        .expect("the boot river upgrades");
    assert_eq!(response.status(), 101);
    let deadline = common::deadline(Duration::from_secs(10));
    while let Ok(Some(Ok(message))) = tokio::time::timeout_at(deadline, socket.next()).await {
        if matches!(message, Message::Text(_)) {
            return;
        }
    }
    panic!("the named boot river produced no frames");
}

#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn a_ws_upgrade_for_a_configured_non_default_symbol_is_served() {
    let venue = spawn(&["--config", &two_symbols_config()]);
    let (mut socket, response) = tokio_tungstenite::connect_async(venue.ws_url_for("MNQ"))
        .await
        .expect("configured non-boot river places a boat");
    assert_eq!(response.status(), 101);
    // Drain to a deadline and record how the stream ended. The shape this
    // replaces - `while let Ok(Some(Ok(Message::Text(frame))))` - exits the
    // loop on a Ping, a Pong, a Binary or a Close as readily as on the
    // deadline, and every one of those then arrived at the panic below as
    // "produced no named market frame". That is the wrong answer rather than a
    // timeout: a venue that closed this socket would be reported as a venue
    // that served an unnamed river. The venue sends no control frames today,
    // so nothing makes it bite yet - which is exactly why it survived, and is
    // no reason to leave the last instance of the shape standing.
    let deadline = common::deadline(Duration::from_secs(10));
    let mut named = false;
    let ending = loop {
        match tokio::time::timeout_at(deadline, socket.next()).await {
            Err(_) => break "the deadline expired with no named market frame".to_string(),
            Ok(None) => break "the venue ended the stream".to_string(),
            Ok(Some(Err(err))) => break format!("the socket failed: {err}"),
            Ok(Some(Ok(Message::Close(frame)))) => {
                break format!("the venue CLOSED the socket: {frame:?}");
            }
            Ok(Some(Ok(Message::Text(frame)))) => {
                if frame.contains("MNQ") {
                    named = true;
                    break String::new();
                }
            }
            // A Ping, a Pong or a Binary frame is not the end of anything.
            Ok(Some(Ok(_))) => {}
        }
    };
    assert!(
        named,
        "configured non-boot river produced no named market frame: {ending}"
    );
}

/// History is bounded by the run clock, and no boat moves that bound.
///
/// The inverse of what this once pinned. The ceiling used to be the furthest
/// boat on the river, which made one passenger's delivery frontier decide
/// another's history window: board the same water at a faster cadence and you
/// moved somebody else's ceiling, which they could watch move. The Boat entry
/// forbids that - nothing a consumer can measure may reveal whether it shares a
/// hull - and a maximum over boats was never a property of the river anyway,
/// since speed belongs to a boat's identity and a tape is what one boat
/// publishes.
///
/// A late boat is the discriminator, and it is why this uses a second symbol.
/// The boot river carries a boat from boot, so a boot-symbol test would pass
/// under either rule. A boat placed well after boot has published almost
/// nothing, so a window sitting far above its frontier but below the run present
/// separates the two rules exactly: the retired ceiling refused it, the run
/// clock serves it.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn history_is_bounded_by_the_run_clock_and_no_boat_moves_it() {
    let venue = spawn(&["--config", &two_symbols_config()]);
    tokio::time::sleep(Duration::from_millis(250)).await;
    let (socket, _) =
        tokio_tungstenite::connect_async(format!("{}?symbol=MNQ&speed=1", venue.ws_url()))
            .await
            .expect("place the second river late");

    let (_, clock) = http_get(&venue.http_base(), "/clock");
    let clock: mogwai_protocol::VenueClock = serde_json::from_str(&clock).unwrap();
    let run_now = clock.venue_now_ns;
    assert!(
        run_now > venue.record.data_origin_ns,
        "the run clock must have left the tape floor for this test to say anything"
    );

    // Just below the run present, and far above anything a boat placed a moment
    // ago can have published - such a boat starts at the river's origin and
    // climbs from there. Under the retired ceiling this was a 400.
    let start = run_now.saturating_sub(1_000_000);
    let (status, body) = http_get(
        &venue.http_base(),
        &format!("/operator/trades?symbol=MNQ&start={start}&limit=5"),
    );
    assert_eq!(
        status, 200,
        "history below the run present must be served whatever any boat has published: {body}"
    );

    // And the bound did not collapse onto nothing: past the run present is still
    // refused. A test asserting only the admission cannot tell a correct bound
    // from one that admits everything, and that is the direction which would let
    // a run read its own future.
    let future = run_now.saturating_add(60_000_000_000);
    let (status, body) = http_get(
        &venue.http_base(),
        &format!("/operator/trades?symbol=MNQ&start={future}&limit=5"),
    );
    assert_eq!(
        status, 400,
        "history past the run present must be refused: {body}"
    );
    drop(socket);
}

#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn an_order_for_another_symbol_is_refused_on_a_bound_socket() {
    let venue = spawn(&["--config", &fast_config()]);
    // A minute of modeled submit-act latency, which this venue's sim clock
    // realizes one-for-one in wall time. The mismatch is refused at the
    // protocol boundary, above that sleep and above the market reading,
    // calendar lookup and engine lock it fronts - so the refusal must land in
    // seconds. This is the assertion that a mismatched order drives no
    // symbol-dependent work: a check placed lower could not answer for a
    // minute, and the deadline below is far short of one.
    assert_eq!(
        post_divergence(
            &venue.http_base(),
            r#"{"type":"CommandLatency","submit_act_ms":60000}"#,
        ),
        202,
        "the act latency is armed"
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(venue.ws_url_for(&venue.symbol))
        .await
        .expect("bind the boot river");
    let submit = r#"{"type":"SubmitOrder","client_order_id":"WRONG-RIVER","symbol":"MES","side":"Buy","order_type":"Limit","quantity":"1","price":"1","time_in_force":"Gtc"}"#;
    socket
        .send(Message::Text(submit.into()))
        .await
        .expect("send mismatched order");

    // Well under the armed 60 s act latency, and generous next to the live tape
    // frames this drains past.
    let deadline = common::deadline(Duration::from_secs(10));
    while let Ok(Some(Ok(message))) = tokio::time::timeout_at(deadline, socket.next()).await {
        let Message::Text(text) = message else {
            continue;
        };
        match serde_json::from_str::<VenueMessage>(&text) {
            Ok(VenueMessage::OrderRejected {
                client_order_id,
                reason,
                ..
            }) if client_order_id == "WRONG-RIVER" => {
                assert!(reason.contains("does not match the symbol this connection is bound to"));
                return;
            }
            Ok(VenueMessage::AdmissionRejected { .. }) => {
                panic!("a symbol mismatch was mislabeled as capacity")
            }
            Ok(VenueMessage::OrderAccepted {
                client_order_id, ..
            }) if client_order_id == "WRONG-RIVER" => {
                panic!("the mismatched order reached the engine")
            }
            _ => {}
        }
    }
    panic!("no mismatch OrderRejected arrived");
}

#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn a_symbol_no_preset_covers_is_served_under_the_default_bundle() {
    let config = format!(
        "{}/tests/configs/unmatched-symbol.toml",
        env!("CARGO_MANIFEST_DIR")
    );
    let venue = spawn(&["--config", &config]);
    let (status, body) = http_get(&venue.http_base(), "/instruments");
    assert_eq!(status, 200, "{body}");
    let defs: Vec<mogwai_protocol::InstrumentDef> = serde_json::from_str(&body).unwrap();
    assert_eq!(defs.len(), 1);
    let def = &defs[0];
    assert_eq!(def.symbol.as_ref(), "FOOBAR");
    let preset = mogwai_venue::config::profile_from_preset("BTCUSDT").unwrap();
    assert_eq!(def.class, preset.def.class);
    assert_eq!(def.price_precision, preset.def.price_precision);
    assert_eq!(def.size_precision, preset.def.size_precision);
    assert_eq!(def.price_increment, preset.def.price_increment);
    assert_eq!(def.size_increment, preset.def.size_increment);

    let (mut socket, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("connect websocket");
    let deadline = common::deadline(common::TEST_WALL_BUDGET);
    while let Ok(Some(Ok(message))) = tokio::time::timeout_at(deadline, socket.next()).await {
        if let Message::Text(text) = message
            && let Ok(VenueMessage::Trade(trade)) = serde_json::from_str(&text)
        {
            assert_eq!(trade.symbol.as_ref(), "FOOBAR");
            return;
        }
    }
    panic!("no FOOBAR trade arrived before the deadline");
}

#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn binary_command_frames_receive_a_protocol_error() {
    let venue = spawn(&["--config", &fast_config()]);
    let (mut socket, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("connect websocket");
    socket
        .send(Message::Binary(
            br#"{"type":"query_orders"}"#.to_vec().into(),
        ))
        .await
        .expect("send binary frame");

    let deadline = common::deadline(Duration::from_secs(10));
    while let Ok(Some(Ok(message))) = tokio::time::timeout_at(deadline, socket.next()).await {
        if let Message::Text(text) = message
            && let Ok(VenueMessage::ProtocolError { reason, .. }) =
                serde_json::from_str::<VenueMessage>(&text)
        {
            assert!(reason.contains("binary command frames are unsupported"));
            return;
        }
    }
    panic!("no ProtocolError arrived before the liveness deadline");
}

// Tape lateness under acceleration is not a test and is not here. It asserted a
// 50 ms p99 on paced delivery, which is a statement about the host rather than
// about this code: a release build failed it at 311 ms under a load average of
// 1.46, and no admission test distinguishes a machine that can judge that budget
// from one that cannot. A gate nobody can evaluate is excluded from every lane
// that would run it, and an excluded gate measures nothing at all.
//
// It is a benchmark instead - `examples/tape_lateness_bench.rs`, registered as a
// measurable target - so the number is recorded on every run against the machine
// and the commit that produced it, which is what makes a regression visible
// without pretending a threshold is portable. `reference/performance.md` keeps
// the readings.

#[test]
#[ignore = "binds a loopback listener"]
fn the_tape_origin_is_fixed_and_independent_of_launch_time() {
    let first = spawn(&["--config", &fast_config()]);
    let second = spawn(&["--config", &fast_config()]);
    for venue in [&first, &second] {
        assert_eq!(venue.record.data_origin_ns, 0);
        assert_eq!(venue.record.run_start_ns, venue.record.warmup_ns);
        let (status, body) = http_get(&venue.http_base(), "/clock");
        assert_eq!(status, 200);
        let clock: mogwai_protocol::VenueClock = serde_json::from_str(&body).unwrap();
        assert_eq!(clock.data_origin_ns, 0);
    }
    assert_eq!(first.record.run_start_ns, second.record.run_start_ns);
    assert_eq!(first.record.warmup_ns, second.record.warmup_ns);
}

#[test]
#[ignore = "binds three loopback listeners"]
fn two_runs_with_the_same_configured_seed_serve_the_same_first_trades() {
    let first = spawn(&["--config", &fast_config()]);
    let second = spawn(&["--config", &fast_config()]);
    let alternate = format!("{}/tests/configs/fast-alt.toml", env!("CARGO_MANIFEST_DIR"));
    let third = spawn(&["--config", &alternate]);
    let page = |venue: &common::Venue| {
        let path = format!(
            "/operator/trades?symbol={}&start=0&end={}&limit=50",
            venue.symbol, venue.record.run_start_ns
        );
        let (status, body) = http_get(&venue.http_base(), &path);
        assert_eq!(status, 200, "{body}");
        body
    };
    let first_page = page(&first);
    assert_eq!(first_page, page(&second));
    assert_ne!(first_page, page(&third));
}

/// The symmetric ceiling. Serving past the clock would be a look-ahead leak no
/// real venue can produce, and an empty `200` would hide it.
#[test]
#[ignore = "binds a loopback listener"]
fn trades_after_sim_now_are_refused_with_400() {
    let venue = spawn(&["--config", &fast_config()]);
    let far_future = venue.record.run_start_ns + 86_400_000_000_000;

    let (status, body) = http_get(
        &venue.http_base(),
        &format!(
            "/operator/trades?symbol={}&start={far_future}",
            venue.symbol
        ),
    );
    assert_eq!(status, 400, "a future start is refused");
    assert!(
        body.contains("sim-now"),
        "the refusal names the clock: {body}"
    );

    // The asymmetry is deliberate and pinned here so it stays a decision: an
    // explicit end past the clock is the ordinary "everything up to now"
    // request written against the caller's own clock, so it is clamped to
    // sim-now and served, not refused. See the comment in `http::trades`.
    let (status, body) = http_get(
        &venue.http_base(),
        &format!(
            "/operator/trades?symbol={}&start={}&end={far_future}&limit=5",
            venue.symbol, venue.record.run_start_ns
        ),
    );
    assert_eq!(status, 200, "a future end is clamped, not refused: {body}");
}

/// Piece 13 replaced the unserved-symbol refusal on history: the only symbol a
/// history read refuses now is one that is not a legal symbol at all. A label
/// nobody configured, and a miscased one, are both served - under their own
/// label, on their own river.
#[test]
#[ignore = "binds a loopback listener"]
fn history_refuses_an_illegal_symbol_and_serves_an_unconfigured_one() {
    let venue = spawn(&["--config", &fast_config()]);
    for endpoint in ["trades", "quotes"] {
        let (status, body) = http_get(
            &venue.http_base(),
            &format!("/operator/{endpoint}?symbol=NOT%20A%20SYMBOL&start=0&limit=5"),
        );
        assert_eq!(status, 400, "an illegal symbol is refused: {body}");
        assert!(
            body.contains("illegal symbol"),
            "the refusal says what is wrong: {body}"
        );

        let (status, body) = http_get(
            &venue.http_base(),
            &format!("/operator/{endpoint}?symbol=NOT-A-SYMBOL&start=0&limit=5"),
        );
        assert_eq!(status, 200, "an unconfigured symbol is served: {body}");
        assert!(
            body.contains("NOT-A-SYMBOL"),
            "the rows wear the requested label: {body}"
        );
    }

    let lowercase = venue.symbol.to_lowercase();
    if lowercase != venue.symbol {
        let (status, body) = http_get(
            &venue.http_base(),
            &format!("/operator/trades?symbol={lowercase}&start=0&limit=5"),
        );
        assert_eq!(status, 200, "a miscased label is its own river: {body}");
        assert!(
            body.contains(&lowercase),
            "and it is served under that label, not folded: {body}"
        );
    }

    let (status, body) = http_get(
        &venue.http_base(),
        &format!("/operator/trades?symbol={}&start=0&limit=5", venue.symbol),
    );
    assert_eq!(
        status, 200,
        "the run's boot symbol remains servable: {body}"
    );
}

/// What proves the warmup was materialized rather than merely declared: a
/// request for the earliest servable instant, issued the moment the readiness
/// line arrives, returns data instead of a refusal or an empty page.
#[test]
#[ignore = "binds a loopback listener"]
fn the_full_warmup_span_is_servable_at_readiness() {
    let venue = spawn(&["--config", &fast_config()]);
    let floor = venue.record.data_origin_ns;

    let (status, body) = http_get(
        &venue.http_base(),
        &format!(
            "/operator/trades?symbol={}&start={floor}&limit=50",
            venue.symbol
        ),
    );
    assert_eq!(status, 200, "the declared floor is servable: {body}");

    let trades: Vec<TradeTick> = serde_json::from_str(&body).expect("a trade page: {body}");
    assert!(
        !trades.is_empty(),
        "the earliest servable instant returned no trades; the warmup was declared but never generated"
    );
    assert!(
        trades[0].ts_event >= floor,
        "the page begins at or after the floor"
    );
    assert!(
        trades[0].ts_event <= venue.record.run_start_ns,
        "the page begins inside the warmup span, not at the live frontier"
    );
}

/// L5: a connection is attached to the run's one tape on upgrade. Nothing is
/// sent by the consumer, and there is no subscribe frame left to send.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn a_connection_receives_the_tape_without_asking() {
    let venue = spawn(&["--config", &fast_config()]);
    let (mut socket, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("open a socket");

    let deadline = common::deadline(common::TEST_WALL_BUDGET);
    loop {
        let message = tokio::time::timeout_at(deadline, socket.next())
            .await
            .expect("the venue pushes its tape unbidden")
            .expect("the socket stays open")
            .expect("a well-formed frame");
        if let Message::Text(text) = message
            && let Ok(VenueMessage::Trade(trade)) = serde_json::from_str(&text)
        {
            assert_eq!(trade.symbol.as_ref(), venue.symbol);
            return;
        }
    }
}

/// A venue declaring no warmup still publishes its tape. The worker's opening
/// positioning probe used to target the simulated now, which a live index will
/// not extend to reach, so the probe's success rested on the warmup walk having
/// overshot the run start far enough to cover the boot latency. With
/// `warmup_ns = 0` there is no overshoot: the worker returned before its first
/// frame, `/health` reported a dead tape, and the venue exited 0 having served
/// nothing. Reverting the probe to the tape origin makes this test time out on
/// the first frame.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn a_venue_without_warmup_still_publishes_its_tape() {
    let venue = spawn(&["--config", &common::no_warmup_config()]);
    let (mut socket, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("open a socket");

    let deadline = common::deadline(common::TEST_WALL_BUDGET);
    loop {
        let message = tokio::time::timeout_at(deadline, socket.next())
            .await
            .expect("a zero-warmup venue still pushes its tape")
            .expect("the socket stays open")
            .expect("a well-formed frame");
        if let Message::Text(text) = message
            && let Ok(VenueMessage::Trade(trade)) = serde_json::from_str(&text)
        {
            assert_eq!(trade.symbol.as_ref(), venue.symbol);
            assert_eq!(venue.record.warmup_ns, 0, "the fixture declares no warmup");
            return;
        }
    }
}

/// The lag policy stays through the tenancy rip - it bounds one connection's
/// memory, not one tenant's share - but the policy itself changed: a connection
/// that falls behind the ring is told what it missed and keeps being served,
/// where it used to be told and then killed with WS 1011.
///
/// What this has to prove is the hard part. "No close" is not the contract and
/// would pass against a venue that simply stopped noticing holes. So this pins
/// three things a silent venue cannot fake: the declaration arrives, the socket
/// goes on delivering market frames after it, and a second hole is declared with
/// a higher episode number - which is what says the venue can still speak about
/// loss once it has spoken once. The old one-shot promise pool could not have
/// passed the third.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn a_slow_connection_is_told_its_gap_and_keeps_being_served() {
    let venue = spawn(&["--config", &tiny_fanout_config()]);
    let (mut socket, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("open a socket");

    // Read slowly rather than not at all, for the same reason as before: a peer
    // that never reads wedges the writer, and the declaration rides that same
    // outbound path. Unlike the old test the stalling never stops - a second
    // episode needs the reader to fall behind twice, so it must keep being slow.
    const STALL: Duration = Duration::from_millis(20);
    let mut episodes: Vec<u64> = Vec::new();
    let mut market_after_first_gap = 0_usize;
    let mut close_code = None;
    let mut reads = 0;
    let deadline = common::deadline(common::TEST_WALL_BUDGET);
    while tokio::time::Instant::now() < deadline && episodes.len() < 2 {
        tokio::time::sleep(STALL).await;
        reads += 1;
        // How the read loop ended is recorded, for the same reason a drain
        // records how its stream ended: a timeout, a transport error and a clean
        // `None` are three different stories and only one is about the policy.
        let ending = match tokio::time::timeout_at(deadline, socket.next()).await {
            Ok(Some(Ok(message))) => match message {
                Message::Text(text) => {
                    match serde_json::from_str::<VenueMessage>(&text) {
                        Ok(VenueMessage::FeedLagged {
                            episode,
                            skipped,
                            skipped_total,
                            ..
                        }) => {
                            assert!(skipped > 0, "a declared hole names a non-zero skip count");
                            assert!(
                                skipped_total >= skipped,
                                "the cumulative total cannot be below this episode: \
                                 {skipped_total} < {skipped}"
                            );
                            episodes.push(episode);
                        }
                        Ok(VenueMessage::Trade(_) | VenueMessage::Quote(_))
                            if !episodes.is_empty() =>
                        {
                            market_after_first_gap += 1;
                        }
                        _ => {}
                    }
                    continue;
                }
                Message::Close(frame) => {
                    close_code = frame.map(|frame| u16::from(frame.code));
                    break;
                }
                _ => continue,
            },
            Err(_) => "the read deadline expired before the venue said anything more",
            Ok(None) => "the venue ended the stream without a close frame",
            Ok(Some(Err(err))) => {
                panic!("the socket failed in transport rather than being served: {err}")
            }
        };
        panic!(
            "{ending}, after {reads} reads with {} declared gaps - so this says nothing about \
             whether the venue keeps serving a passenger it has told about a hole",
            episodes.len()
        );
    }

    assert!(
        close_code.is_none(),
        "a declared hole is advisory: the venue must not close the socket, got {close_code:?}"
    );
    assert_eq!(
        episodes.len(),
        2,
        "expected two declared gaps in {reads} reads; a venue that declares once and goes quiet \
         looks identical to one that stopped noticing"
    );
    assert!(
        episodes[1] > episodes[0],
        "each declaration carries a rising episode: {episodes:?}"
    );
    assert!(
        market_after_first_gap > 0,
        "the venue must go on delivering market frames after declaring a hole; none arrived \
         between the first declaration and the second"
    );
}

/// L6: one process is one ledger. Two connections must see the same account,
/// and an order worked on one must be visible to the other.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn two_connections_share_one_ledger() {
    let venue = spawn(&["--config", &fast_config()]);
    let (mut worker, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("open the order socket");
    let (mut observer, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("open the observing socket");

    let submit = format!(
        r#"{{"type":"SubmitOrder","client_order_id":"SHARED-1","symbol":"{}","side":"Buy","order_type":"Market","quantity":"1","time_in_force":"Gtc"}}"#,
        venue.symbol
    );
    worker
        .send(Message::Text(submit.into()))
        .await
        .expect("submit an order");

    // The order is worked on one socket; the ledger it moved is the run's, so
    // the other socket's query answers from the same book.
    //
    // The query waits on the acceptance, not on a fixed 500 ms. The query is
    // sent once and never retried, so a submit still in flight when it went out
    // produced a snapshot without `SHARED-1` and a failure reading "the run has
    // more than one ledger" - a wrong answer about a venue that was merely busy.
    //
    // `observer` is drained for that whole wait. It is attached to the live tape
    // too, so leaving it parked would make an eviction the next thing this test
    // saw, reported as the ledger property failing.
    while_draining(
        &mut observer,
        await_acceptance(&mut worker, "SHARED-1"),
        "the observing socket",
    )
    .await;
    observer
        .send(Message::Text(
            r#"{"type":"QueryOrders","request_id":"Q-1","open_only":false}"#.into(),
        ))
        .await
        .expect("query the venue's truth");

    let deadline = common::deadline(common::TEST_WALL_BUDGET);
    loop {
        let message = tokio::time::timeout_at(deadline, observer.next())
            .await
            .expect("the query is answered")
            .expect("the socket stays open")
            .expect("a well-formed frame");
        if let Message::Text(text) = message
            && let Ok(VenueMessage::OrderStatusSnapshot(snapshot)) =
                serde_json::from_str::<VenueMessage>(&text)
        {
            assert_eq!(snapshot.request_id, "Q-1");
            assert!(
                snapshot
                    .orders
                    .iter()
                    .any(|order| order.client_order_id == "SHARED-1"),
                "the second connection cannot see the first's order: the run has \
                 more than one ledger"
            );
            return;
        }
    }
}

/// Two accounts on one venue do not share a ledger, which is the converse of
/// the test above and the property that makes a shared exchange usable.
///
/// The pair matters together: same account id means one trader and one book
/// (above), different ids mean two traders who cannot see or move each other
/// (here). Before the per-account ledger the engine was one per process, so
/// this assertion failed - the observer saw the worker's order, and their
/// balances moved together.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn two_accounts_on_one_venue_do_not_share_a_ledger() {
    let venue = spawn(&["--config", &fast_config()]);
    let account_url = |account: &str| format!("{}?account={account}", venue.ws_url());
    let (mut worker, _) = tokio_tungstenite::connect_async(account_url("WYRD-001"))
        .await
        .expect("open the first account's socket");
    let (mut stranger, _) = tokio_tungstenite::connect_async(account_url("WYRD-002"))
        .await
        .expect("open the second account's socket");

    let submit = format!(
        r#"{{"type":"SubmitOrder","client_order_id":"PRIVATE-1","symbol":"{}","side":"Buy","order_type":"Market","quantity":"1","time_in_force":"Gtc"}}"#,
        venue.symbol
    );
    worker
        .send(Message::Text(submit.into()))
        .await
        .expect("submit an order on the first account");

    // As above, but here the fixed wait was worse than a wrong answer: this
    // assertion is that the stranger's book does not contain `PRIVATE-1`, and an
    // order the venue had not booked yet satisfies it vacuously. Waiting for the
    // acceptance is what makes the absence mean something.
    //
    // And `stranger` is drained across the wait, for the reason above: an
    // absence proved on an evicted socket is not an absence.
    while_draining(
        &mut stranger,
        await_acceptance(&mut worker, "PRIVATE-1"),
        "the second account's socket",
    )
    .await;
    stranger
        .send(Message::Text(
            r#"{"type":"QueryOrders","request_id":"Q-2","open_only":false}"#.into(),
        ))
        .await
        .expect("query the second account's truth");

    let deadline = common::deadline(common::TEST_WALL_BUDGET);
    loop {
        let message = tokio::time::timeout_at(deadline, stranger.next())
            .await
            .expect("the query is answered")
            .expect("the socket stays open")
            .expect("a well-formed frame");
        if let Message::Text(text) = message
            && let Ok(VenueMessage::OrderStatusSnapshot(snapshot)) =
                serde_json::from_str::<VenueMessage>(&text)
        {
            assert_eq!(snapshot.request_id, "Q-2");
            assert!(
                snapshot
                    .orders
                    .iter()
                    .all(|order| order.client_order_id != "PRIVATE-1"),
                "one account can see another's order: the venue has one ledger \
                 where it should have two"
            );
            return;
        }
    }
}

/// A consumer names its own opening balance, and that is the ledger it trades.
///
/// The venue's `[balances]` is what an unnamed account gets; it stops being the
/// balance of the one ledger. Two experiments sized differently are the case
/// this exists for, and they have to be runnable on one venue.
#[test]
#[ignore = "binds a loopback listener"]
fn an_account_opens_on_the_balance_its_consumer_named() {
    let venue = spawn(&["--config", &fast_config()]);
    let (status, body) = http_post_json(
        &venue.http_base(),
        "/accounts",
        r#"{"account_id":"WYRD-100","balances":{"USDT":"250000"}}"#,
    );
    assert_eq!(status, 201, "the account opens: {body}");

    let (status, body) = http_get(&venue.http_base(), "/account?account=WYRD-100");
    assert_eq!(status, 200, "the named account answers: {body}");
    let named: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(named["account_id"], "WYRD-100");
    assert!(
        body.contains("250000"),
        "the consumer's opening balance is the ledger's: {body}"
    );

    // The default account is untouched by it, which is what makes the two
    // separable rather than one ledger wearing a different label.
    let (status, body) = http_get(&venue.http_base(), "/account");
    assert_eq!(status, 200, "the default account still answers: {body}");
    let default: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_ne!(
        default["account_id"], "WYRD-100",
        "the default account is a different ledger: {body}"
    );
    assert_ne!(
        default["balances"], named["balances"],
        "a consumer-named balance leaked into the default account: {body}"
    );
}

/// Re-opening a live account is refused rather than resetting it. An account
/// outlives its connections, so the request is ambiguous between a fresh
/// experiment and a reconnecting consumer re-sending its config - and the second
/// reading would silently wipe a position book.
#[test]
#[ignore = "binds a loopback listener"]
fn an_account_that_is_already_open_is_not_reset() {
    let venue = spawn(&["--config", &fast_config()]);
    let (status, body) = http_post_json(
        &venue.http_base(),
        "/accounts",
        r#"{"account_id":"WYRD-101","balances":{"USDT":"1000"}}"#,
    );
    assert_eq!(status, 201, "the account opens: {body}");
    let (status, body) = http_post_json(
        &venue.http_base(),
        "/accounts",
        r#"{"account_id":"WYRD-101","balances":{"USDT":"9999"}}"#,
    );
    assert_eq!(status, 409, "re-opening is refused: {body}");

    let (status, body) = http_get(&venue.http_base(), "/account?account=WYRD-101");
    assert_eq!(status, 200, "the account answers: {body}");
    assert!(
        body.contains("1000") && !body.contains("9999"),
        "the refused re-open must not have moved the balance: {body}"
    );
}

/// A policed account publishes what it is being enforced against, so a run can
/// be judged afterwards.
///
/// The audience is the evaluator, not the strategy: mogwai presents no
/// dashboard, so a run that ended flat having spent most of its drawdown budget
/// is indistinguishable from one that never came close unless these numbers are
/// on the wire.
#[test]
#[ignore = "binds a loopback listener"]
fn a_policed_account_publishes_its_remaining_budget() {
    let venue = spawn(&["--config", &fast_config()]);
    let (status, body) = http_post_json(
        &venue.http_base(),
        "/accounts",
        r#"{"account_id":"WYRD-200","balances":{"USDT":"50000"},
            "policy":{"currency":"USDT",
                      "trailing_drawdown":{"amount":"2000"},
                      "daily_loss_limit":{"amount":"500"}}}"#,
    );
    assert_eq!(status, 201, "the policed account opens: {body}");

    let (status, body) = http_get(&venue.http_base(), "/account?account=WYRD-200");
    assert_eq!(status, 200, "the account answers: {body}");
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    let risk = &value["risk"];
    assert_eq!(
        risk["peak_equity"], "50000",
        "the high-water mark starts at the opening balance: {body}"
    );
    assert_eq!(
        risk["trailing_threshold"], "48000",
        "the floor is the peak less the allowance: {body}"
    );
    assert_eq!(
        risk["trailing_remaining"], "2000",
        "nothing is spent before anything trades: {body}"
    );
    assert_eq!(
        risk["daily_remaining"], "500",
        "the daily budget starts whole: {body}"
    );
    assert!(risk["breached"].is_null(), "nothing has fired yet: {body}");
}

/// A policy the venue cannot enforce is refused where it enters, not hours
/// later. A nonsense rule that booted fine and then behaved strangely would be
/// the worst of both.
#[test]
#[ignore = "binds a loopback listener"]
fn an_unenforceable_policy_is_refused_at_the_boundary() {
    let venue = spawn(&["--config", &fast_config()]);
    let (status, body) = http_post_json(
        &venue.http_base(),
        "/accounts",
        r#"{"account_id":"WYRD-201","balances":{"USDT":"50000"},
            "policy":{"currency":"USDT","trailing_drawdown":{"amount":"0"}}}"#,
    );
    assert_eq!(status, 400, "a zero drawdown is refused: {body}");
    assert!(
        body.contains("trailing_drawdown.amount"),
        "the refusal names the field: {body}"
    );

    let (status, body) = http_post_json(
        &venue.http_base(),
        "/accounts",
        r#"{"account_id":"WYRD-202","balances":{"USDT":"50000"},
            "policy":{"currency":"USDT","reset_minute_utc":1440}}"#,
    );
    assert_eq!(status, 400, "a reset outside the day is refused: {body}");

    // A rule with no currency has no meaning: the threshold would be stated in
    // nothing, and the venue has no rate to pick one with.
    let (status, body) = http_post_json(
        &venue.http_base(),
        "/accounts",
        r#"{"account_id":"WYRD-203","balances":{"USDT":"50000"},
            "policy":{"daily_loss_limit":{"amount":"500"}}}"#,
    );
    assert_eq!(
        status, 400,
        "a policy without a currency is refused: {body}"
    );
    assert!(
        body.contains("currency"),
        "the refusal names what is missing: {body}"
    );
}

/// A policed spot account trades and is valued: the base asset it ends up
/// holding is priced by the pair that quotes it.
///
/// This is what the default tape shape needs. A spot fill credits the base as a
/// currency balance and debits the quote, so an account holding BTC is worth
/// nothing statable until BTCUSDT is marked - and its equity must never collapse
/// by the notional it just spent, which is what a naive sum of balances
/// reported before the valuation landed.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn a_policed_spot_account_is_valued_at_the_marked_price() {
    let venue = spawn(&["--config", &fast_config()]);
    let (status, body) = http_post_json(
        &venue.http_base(),
        "/accounts",
        r#"{"account_id":"WYRD-204","balances":{"USDT":"5000000"},
            "policy":{"currency":"USDT","trailing_drawdown":{"amount":"1000000"}}}"#,
    );
    assert_eq!(status, 201, "the policed account opens: {body}");

    let (mut socket, _) =
        tokio_tungstenite::connect_async(format!("{}?account=WYRD-204", venue.ws_url()))
            .await
            .expect("open the policed account's socket");
    let submit = format!(
        r#"{{"type":"SubmitOrder","client_order_id":"SPOT-1","symbol":"{}","side":"Buy","order_type":"Market","quantity":"1","time_in_force":"Gtc"}}"#,
        venue.symbol
    );
    socket
        .send(Message::Text(submit.into()))
        .await
        .expect("submit a spot order");

    // The purchase must actually book before anything is asked about its
    // valuation, and the order of the two waits is what makes this test able to
    // fail at all: the account opens with 5,000,000 USDT, so the equity floor
    // below is already satisfied before the fill lands. A poll that did not wait
    // for the fill first would pass on the opening balance.
    //
    // Waiting for the fill on the wire also keeps the socket drained. It sat
    // unread across the old 1.5 s sleep, and on this unpaced tape an unread
    // socket is eventually ejected by the bounded fanout ring.
    let deadline = common::deadline(common::TEST_WALL_BUDGET);
    loop {
        let message = tokio::time::timeout_at(deadline, socket.next())
            .await
            .expect("the spot buy fills within 30 s")
            .expect("the venue CLOSED the policed account's socket before the buy filled")
            .expect("a well-formed frame");
        if let Message::Text(text) = message
            && let Ok(VenueMessage::OrderFilled(fill)) = serde_json::from_str::<VenueMessage>(&text)
            && fill.client_order_id == "SPOT-1"
        {
            break;
        }
    }
    let drain = BackgroundDrain::spawn(socket);

    // Then the mark. A sweep pass valuing the pair is what lifts equity back
    // over the floor, and it runs on its own cadence - so this retries to a
    // deadline rather than betting a fixed span was enough. A venue that never
    // marks fails here with the reading it actually served, instead of the
    // reading whichever host it ran on happened to reach in 1.5 s.
    let deadline = common::deadline(common::TEST_WALL_BUDGET);
    let body = loop {
        let (status, body) = http_get(&venue.http_base(), "/account?account=WYRD-204");
        assert_eq!(status, 200, "the account answers: {body}");
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        let equity: f64 = value["risk"]["equity"]
            .as_str()
            .expect("equity is reported")
            .parse()
            .expect("equity parses");
        if equity > 4_000_000.0 {
            break body;
        }
        drain.assert_still_serving("the policed account's socket");
        assert!(
            tokio::time::Instant::now() < deadline,
            "buying an asset must not read as spending its notional; 30 s after the fill the \
             account still reads: {body}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    drain.stop("the policed account's socket").await;
    // The retry above cannot hide a transient mispricing, and that is a property
    // of the breach rather than of the loop. `RiskLedger::breach` is latched -
    // `observe` returns early once it is set, and the first breach is the one
    // that describes the run - so an equity reading that momentarily collapsed
    // by the notional would have crossed this account's 1,000,000 trailing
    // drawdown off its 5,000,000 opening and stuck there. The poll can wait for the
    // mark; it cannot wait out a breach that already fired.
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        value["risk"]["breached"].is_null(),
        "a purchase is not a drawdown breach: {body}"
    );
}

/// A second socket claiming an existing account evicts the first, and resumes its
/// ledger.
///
/// Both halves matter together. Eviction is what keeps an account on one river
/// with one reader: two sockets on one id would be one ledger written from two
/// places. Resuming is what makes a reconnect a continuation - the venue cannot
/// tell a returning consumer from a stranger presenting the id, so handing the
/// ledger over is the only behaviour that lets a killed worker come back to its
/// own position.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn a_second_socket_claiming_an_account_evicts_the_first_and_resumes_its_ledger() {
    let venue = spawn(&["--config", &fast_config()]);
    let url = format!("{}?account=WYRD-300", venue.ws_url());
    let (mut first, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("open the first socket");

    let submit = format!(
        r#"{{"type":"SubmitOrder","client_order_id":"RESUMED-1","symbol":"{}","side":"Buy","order_type":"Market","quantity":"1","time_in_force":"Gtc"}}"#,
        venue.symbol
    );
    first
        .send(Message::Text(submit.into()))
        .await
        .expect("submit an order on the first socket");
    tokio::time::sleep(Duration::from_millis(500)).await;

    let (mut second, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("open the second socket on the same account");

    // The query goes out before the eviction is observed, and both sockets are
    // then read concurrently, each on its own deadline.
    //
    // Sequencing them shared one 30 s instant across two phases, so a slow
    // eviction ate the query's budget and the timeout fired with a message about
    // the ledger. Worse, `second` sat unread for the whole of the first phase:
    // this venue's fanout is a bounded ring, so a socket nobody reads loses
    // market frames and is told so - it is no longer closed for it, but a
    // `None` here would still have been reported as the resumed ledger failing
    // to answer. Each arm below therefore names its
    // own failure, and a venue-side close is spelled out as a close rather than
    // folded into "the query went unanswered".
    second
        .send(Message::Text(
            r#"{"type":"QueryOrders","request_id":"Q-3","open_only":false}"#.into(),
        ))
        .await
        .expect("query the resumed ledger");

    // The incumbent is closed, and normally: an eviction is not a fault, and a
    // consumer that redialled on it would evict whatever evicted it.
    let evicted = async {
        let deadline = common::deadline(common::TEST_WALL_BUDGET);
        loop {
            let message = tokio::time::timeout_at(deadline, first.next())
                .await
                .map_err(|_| {
                    "the incumbent socket was still open 30 s after another claimed its account"
                        .to_string()
                })?
                .ok_or_else(|| {
                    "the incumbent stream ended without a close frame, so the eviction carried no \
                     reason"
                        .to_string()
                })?
                .map_err(|err| format!("the incumbent socket failed in transport: {err}"))?;
            if let Message::Close(frame) = message {
                return Ok::<_, String>(frame);
            }
        }
    };

    // And the newcomer inherits the book rather than a fresh one.
    let resumed = async {
        let deadline = common::deadline(common::TEST_WALL_BUDGET);
        loop {
            let message = tokio::time::timeout_at(deadline, second.next())
                .await
                .map_err(|_| "the resumed socket did not answer Q-3 within 30 s".to_string())?
                .ok_or_else(|| {
                    "the venue CLOSED the resuming socket before it answered Q-3, so this run says \
                     nothing about whether the ledger resumes"
                        .to_string()
                })?
                .map_err(|err| format!("the resumed socket failed in transport: {err}"))?;
            if let Message::Text(text) = message
                && let Ok(VenueMessage::OrderStatusSnapshot(snapshot)) =
                    serde_json::from_str::<VenueMessage>(&text)
                && snapshot.request_id == "Q-3"
            {
                return Ok::<_, String>(snapshot);
            }
        }
    };

    let (evicted, resumed) = tokio::join!(evicted, resumed);
    let frame = evicted.unwrap_or_else(|why| panic!("{why}"));
    let frame = frame.expect("the close carries a reason");
    assert_eq!(
        u16::from(frame.code),
        1000,
        "an eviction closes normally, not as a fault: {frame:?}"
    );
    assert!(
        frame.reason.contains("claimed account"),
        "the close says why: {frame:?}"
    );
    // The half of the contract the reason text cannot state. WS 1000 is also
    // what a completed run and an elapsed duration close with, so a consumer can
    // only tell an eviction apart by the protocol prefix - and this is the only
    // place the venue's real bytes meet the classifier that reads them. An
    // assertion on the prose alone passes whether or not the prefix is there,
    // which would leave the whole contract pinned on the consumer's side.
    assert!(
        frame
            .reason
            .starts_with(mogwai_protocol::close::EVICTED_PREFIX),
        "the close is not machine-classifiable as an eviction: {frame:?}"
    );
    assert_eq!(
        mogwai_protocol::close::classify(u16::from(frame.code), &frame.reason),
        Some(mogwai_protocol::close::Terminal::Evicted),
        "the venue's own close frame does not classify as an eviction: {frame:?}"
    );

    let snapshot = resumed.unwrap_or_else(|why| panic!("{why}"));
    assert!(
        snapshot
            .orders
            .iter()
            .any(|order| order.client_order_id == "RESUMED-1"),
        "the returning connection did not get its own ledger back"
    );
}

/// A consumer asks for a policy by name rather than restating it, and a name
/// nobody has is an error rather than a silent fall to unpoliced.
///
/// The second half is the one that matters: a run that believes it is enforced
/// and is not is worse than either being enforced or being told it is not.
#[test]
#[ignore = "binds a loopback listener"]
fn a_policy_preset_resolves_by_name_and_an_unknown_one_is_refused() {
    let venue = spawn(&["--config", &fast_config()]);
    let (status, body) = http_post_json(
        &venue.http_base(),
        "/accounts",
        r#"{"account_id":"WYRD-400","balances":{"USD":"50000"},
            "policy_preset":"intraday-trail"}"#,
    );
    assert_eq!(status, 201, "the named policy resolves: {body}");
    let (status, body) = http_get(&venue.http_base(), "/account?account=WYRD-400");
    assert_eq!(status, 200, "the account answers: {body}");
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        value["risk"]["trailing_threshold"], "48000",
        "the shipped intraday trail is a 2,000 drawdown: {body}"
    );

    let (status, body) = http_post_json(
        &venue.http_base(),
        "/accounts",
        r#"{"account_id":"WYRD-401","balances":{"USD":"50000"},
            "policy_preset":"apex-nonesuch-50k"}"#,
    );
    assert_eq!(status, 400, "an unknown policy name is refused: {body}");
    assert!(
        body.contains("apex-nonesuch-50k") && body.contains("intraday-trail"),
        "the refusal names what was asked for and what exists: {body}"
    );

    let (status, body) = http_post_json(
        &venue.http_base(),
        "/accounts",
        r#"{"account_id":"WYRD-402","balances":{"USD":"50000"},
            "policy_preset":"static-drawdown"}"#,
    );
    assert_eq!(status, 201, "the static ruleset resolves: {body}");
    let (status, body) = http_get(&venue.http_base(), "/account?account=WYRD-402");
    assert_eq!(status, 200, "the static account answers: {body}");
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        value["risk"]["overall_threshold"], "45000",
        "a 5,000 static floor off a 50k open: {body}"
    );
    assert_eq!(
        value["risk"]["max_position"],
        serde_json::Value::Null,
        "static-drawdown does not cap size: {body}"
    );

    let (status, body) = http_post_json(
        &venue.http_base(),
        "/accounts",
        r#"{"account_id":"WYRD-403","balances":{"USD":"50000"},
            "policy_preset":"intraday-trail-sized"}"#,
    );
    assert_eq!(status, 201, "the sized ruleset resolves: {body}");
    let (status, body) = http_get(&venue.http_base(), "/account?account=WYRD-403");
    assert_eq!(status, 200, "the sized account answers: {body}");
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        value["risk"]["max_position"], "10",
        "the sized ruleset publishes its cap: {body}"
    );
}

/// An order that would take the book past the policy's position cap is refused
/// at entry, by name. The firm would not have taken it, so the venue must not
/// either - flattening after the fact would be the wrong story.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn an_oversized_submit_is_refused_by_the_position_cap() {
    // MNQ settles in USD, so a USD-policed account can hold it. The default
    // tape is a spot pair that would credit a second currency and be refused
    // for that reason first.
    let venue = spawn(&["--config", &mnq_preset_config()]);
    let (status, body) = http_post_json(
        &venue.http_base(),
        "/accounts",
        r#"{"account_id":"WYRD-600","balances":{"USD":"1000000"},
            "policy":{"currency":"USD","max_position":{"quantity":"10"}}}"#,
    );
    assert_eq!(status, 201, "the capped account opens: {body}");

    let (mut socket, _) = tokio_tungstenite::connect_async(format!(
        "{}&account=WYRD-600",
        venue.ws_url_for(&venue.symbol)
    ))
    .await
    .expect("bind the capped account");

    let submit = format!(
        r#"{{"type":"SubmitOrder","client_order_id":"TOO-BIG","symbol":"{}","side":"Buy","order_type":"Limit","quantity":"11","price":"1","time_in_force":"Gtc"}}"#,
        venue.symbol
    );
    socket
        .send(Message::Text(submit.into()))
        .await
        .expect("send the oversized order");

    let deadline = common::deadline(Duration::from_secs(10));
    while let Ok(Some(Ok(message))) = tokio::time::timeout_at(deadline, socket.next()).await {
        let Message::Text(text) = message else {
            continue;
        };
        match serde_json::from_str::<VenueMessage>(&text) {
            Ok(VenueMessage::OrderRejected { reason, .. }) => {
                assert!(
                    reason.contains("may not carry more than 10"),
                    "the refusal names the cap: {reason}"
                );
                return;
            }
            Ok(VenueMessage::OrderAccepted { .. }) => {
                panic!("an oversized submit must not be accepted")
            }
            Ok(VenueMessage::AdmissionRejected {
                subject, reason, ..
            }) => panic!("the venue refused admission for {subject:?}: {reason}"),
            Ok(VenueMessage::ProtocolError { reason, .. }) => {
                panic!("the venue read the submit as malformed: {reason}")
            }
            _ => {}
        }
    }
    panic!("no position-cap OrderRejected arrived");
}

/// A blackout armed on one account does not blind another.
///
/// Transport havoc corrupts what one connection receives rather than what the
/// generator produces, so it is armed on the account and blurs each of its
/// passengers alike. Run-wide was the old shape,
/// and on a shared exchange it meant one subagent arming a blackout blacked out
/// the whole batch.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn a_blackout_armed_on_one_account_leaves_another_seeing() {
    let venue = spawn(&["--config", &fast_config()]);
    let (mut dark, _) =
        tokio_tungstenite::connect_async(format!("{}?account=WYRD-500", venue.ws_url()))
            .await
            .expect("open the account to be blacked out");
    let (mut lit, _) =
        tokio_tungstenite::connect_async(format!("{}?account=WYRD-501", venue.ws_url()))
            .await
            .expect("open the account that must keep seeing");

    let (status, body) = http_post_json(
        &venue.http_base(),
        "/control/divergence",
        // The ceiling, in simulated ms. This config is accelerated, so a
        // wall-comfortable window has to be a large sim one.
        r#"{"type":"GoDark","ms":3600000,"account":"WYRD-500"}"#,
    );
    assert_eq!(status, 202, "the targeted blackout arms: {body}");

    // Both sockets are drained before either is judged. A socket is attached to
    // the live tape on upgrade, so whatever the writer had queued when the
    // blackout armed is still in flight on both of them and says nothing about
    // whether a window is open. Asserting before this drain passes whichever way
    // the targeting is wired, which is the trap this comment exists to mark.
    // Whether a socket goes quiet is the discriminator, and it has to be, because
    // neither "did it receive something" nor "was the next frame absent" can
    // tell the two apart: a socket is attached to the live tape on upgrade, so a
    // blacked-out one keeps delivering its backlog for a while and a live one
    // never goes quiet at all. Reading each to a gap gives a clean answer -
    // blacked out means the backlog exhausts and a gap appears, still served
    // means frames keep arriving until the deadline.
    //
    // The two are drained concurrently, and the observer is three-valued rather
    // than a bool. Both properties are load-bearing:
    //
    // - Draining one while the other sits unread let the unread one rot. This
    //   venue's fanout is a bounded ring, so a socket nobody reads accumulates
    //   declared holes in its market view, and the quiet that follows would then
    //   be read as the blackout's doing.
    // - A two-valued "did it go quiet" cannot see that ejection at all. A closed
    //   stream answers `next()` with `None` immediately, which is not a timeout,
    //   so a bool observer reports "still receiving" for a socket that is gone -
    //   and `lit` would pass this gate while dead. `Closed` is therefore its own
    //   verdict, and it fails both halves with a message that names the eviction
    //   instead of blaming the blackout's targeting.
    let (dark_saw, lit_saw) = tokio::join!(
        observe(&mut dark, Duration::from_secs(5)),
        observe(&mut lit, Duration::from_secs(5)),
    );
    assert_eq!(
        dark_saw,
        Observed::Quiet,
        "the targeted account was expected to fall silent inside its blackout; it {}",
        dark_saw.describe()
    );
    assert_eq!(
        lit_saw,
        Observed::Serving,
        "the untargeted account was expected to keep receiving; it {}",
        lit_saw.describe()
    );
}

/// What draining a `/ws` socket to a deadline actually observed.
///
/// Three-valued on purpose: `Closed` is never `Quiet`. A socket the venue has
/// ejected returns `None` from `next()` at once, so any observer that folds the
/// two together reports silence for a socket that no longer exists.
#[derive(Debug, PartialEq, Eq)]
enum Observed {
    /// Frames kept arriving for the whole window - the socket never gapped.
    Serving,
    /// The socket stayed open and produced nothing for a whole gap slice.
    Quiet,
    /// The stream ended: a close frame, a transport error, or end of stream.
    Closed(String),
}

impl Observed {
    fn describe(&self) -> String {
        match self {
            Self::Serving => "kept receiving frames".to_string(),
            Self::Quiet => "went quiet".to_string(),
            Self::Closed(why) => {
                format!("was closed by the venue, which says nothing about the blackout: {why}")
            }
        }
    }
}

/// Drains `socket` for `window`, reporting which of the three states it is in.
/// A quarter-second without a frame counts as a gap, which on an unpaced tape is
/// several orders of magnitude longer than the inter-frame spacing.
async fn observe(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    window: Duration,
) -> Observed {
    let deadline = tokio::time::Instant::now() + window;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(250), socket.next()).await {
            Err(_) => return Observed::Quiet,
            Ok(None) => return Observed::Closed("the stream ended".to_string()),
            Ok(Some(Err(err))) => return Observed::Closed(format!("transport error: {err}")),
            Ok(Some(Ok(Message::Close(frame)))) => {
                return Observed::Closed(format!("close frame {frame:?}"));
            }
            Ok(Some(Ok(_))) => {}
        }
    }
    Observed::Serving
}

/// Two accounts asking for the same river at different speeds both stay
/// open. Speed is a cursor, not a refusal: the second is a cache miss on
/// the sharing key, not a conflict with the first.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn two_accounts_at_different_speeds_both_stay_open() {
    let venue = spawn(&["--config", &fast_config()]);
    let (slow, _) = tokio_tungstenite::connect_async(format!(
        "{}&account=WYRD-700&speed=2",
        venue.ws_url_for(&venue.symbol)
    ))
    .await
    .expect("open the slow cursor");
    let (fast, _) = tokio_tungstenite::connect_async(format!(
        "{}&account=WYRD-701&speed=3",
        venue.ws_url_for(&venue.symbol)
    ))
    .await
    .expect("an unserved speed is a second boat, not a 400");

    // Each socket's own delivery is the observable. This used to read
    // `/clock?symbol=&speed=` for each boat and assert the two cadences back,
    // which is precisely the boat-discovery the route no longer performs - and
    // it proved the registry held two entries rather than that either passenger
    // was being served. Being served is the property the test is named for, and
    // a socket is entitled to know its own frames arrived.
    let mut slow = slow;
    let mut fast = fast;
    for (label, socket) in [
        ("the slow cursor", &mut slow),
        ("the fast cursor", &mut fast),
    ] {
        let deadline = common::deadline(common::TEST_WALL_BUDGET);
        let mut served = false;
        while !served {
            let message = tokio::time::timeout_at(deadline, socket.next())
                .await
                .unwrap_or_else(|_| panic!("{label} was given no market data before the deadline"));
            match message {
                Some(Ok(Message::Text(text))) => {
                    served = matches!(
                        serde_json::from_str::<VenueMessage>(&text),
                        Ok(VenueMessage::Trade(_) | VenueMessage::Quote(_))
                    );
                }
                Some(Ok(Message::Close(frame))) => {
                    panic!("{label} was closed rather than served: {frame:?}")
                }
                Some(Ok(_)) => {}
                Some(Err(err)) => panic!("{label} failed in transport: {err}"),
                None => panic!("{label} ended without a close frame"),
            }
        }
    }
}

/// One ledger still carries one cadence. A second socket on the same account
/// asking for a different speed of a river it is already riding is refused,
/// because that would be two clocks judging one book.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn a_second_speed_on_the_same_account_is_refused() {
    let venue = spawn(&["--config", &fast_config()]);
    // Unnamed sockets share the default account and do not evict, so this is
    // two live connections on one ledger rather than a reconnect.
    let (_first, _) =
        tokio_tungstenite::connect_async(format!("{}&speed=2", venue.ws_url_for(&venue.symbol)))
            .await
            .expect("open the first cadence");
    let refused =
        tokio_tungstenite::connect_async(format!("{}&speed=3", venue.ws_url_for(&venue.symbol)))
            .await;
    let Err(error) = refused else {
        panic!("a second cadence on one ledger must not upgrade");
    };
    // The reason, not merely a 400. `contains("400") || contains("already
    // seated")` admitted any bad request on this route - the illegal-symbol
    // refusal, an unfunded-account refusal, a malformed speed - so a venue that
    // had stopped checking the cadence entirely could still turn this green by
    // refusing for some other reason. The status and the body are read off the
    // structured refusal instead.
    let tokio_tungstenite::tungstenite::Error::Http(response) = error else {
        panic!("the refusal is an HTTP status before the upgrade, got {error}");
    };
    assert_eq!(
        response.status(),
        400,
        "a second cadence is refused before the 101: {response:?}"
    );
    let body = String::from_utf8_lossy(response.body().as_deref().unwrap_or_default()).into_owned();
    assert!(
        body.contains("already seated"),
        "the refusal is the cadence check rather than some other 400: {body}"
    );
    assert!(
        body.contains(&*venue.symbol) && body.contains("speed 2"),
        "the refusal names the river and the sitting cadence, not the asked one: {body}"
    );
}

/// A refused upgrade does not evict the incumbent it was refused instead of
/// replacing.
///
/// `/ws` used to claim the account first - which closes every socket of the
/// incumbent callsign and,
/// under the reset knob, discards its ledger - and only then run its five
/// refusals, so `?account=X&speed=NaN` was a one-request, unauthenticated way
/// to disconnect a live consumer while never connecting at all.
///
/// Two phases, and the second is the point. The refusal path and the admission
/// path differ in exactly one character of the query string, so the incumbent
/// surviving phase one can only mean the refusal spared it: phase two runs the
/// same claim on the same account under the same new callsign with a legal
/// speed, and the incumbent must then be evicted. Without it, a venue that had
/// stopped evicting altogether - or one whose eviction never reached this
/// socket - would pass phase one for the wrong reason.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn a_refused_upgrade_leaves_the_incumbent_connected() {
    let venue = spawn(&["--config", &fast_config()]);
    let (mut incumbent, _) = tokio_tungstenite::connect_async(format!(
        "{}&account=WYRD-820&callsign=alpha&speed=1",
        venue.ws_url_for(&venue.symbol)
    ))
    .await
    .expect("the incumbent claims the account");

    // Wait for the incumbent to be bound before claiming its account, and read
    // an observable rather than sleeping: a `connect_async` returns at the 101,
    // and `handle_socket` binds the lane the eviction has to find only after
    // that. A market-data frame is written by the feed task, which is spawned
    // after `bind_lanes`, so one frame proves the lane is there - and without
    // this the eviction has nothing to close and the test passes for a reason
    // that has nothing to do with the ordering under test.
    let bound = common::deadline(Duration::from_secs(10));
    loop {
        match tokio::time::timeout_at(bound, incumbent.next()).await {
            Err(_) => panic!("the incumbent never received a frame, so it never bound its lane"),
            Ok(Some(Ok(Message::Text(_)))) => break,
            Ok(None | Some(Err(_) | Ok(Message::Close(_)))) => {
                panic!("the venue ended the incumbent's socket before the test began")
            }
            Ok(Some(Ok(_))) => {}
        }
    }

    // A different callsign, so this is a stranger claiming the id - the shape
    // that evicts - and it is refused for the speed alone.
    let refused = while_draining(
        &mut incumbent,
        tokio_tungstenite::connect_async(format!(
            "{}&account=WYRD-820&callsign=beta&speed=NaN",
            venue.ws_url_for(&venue.symbol)
        )),
        "the incumbent socket",
    )
    .await;
    let Err(error) = refused else {
        panic!("a non-finite speed must not upgrade");
    };
    let tokio_tungstenite::tungstenite::Error::Http(response) = error else {
        panic!("the refusal is an HTTP status before the upgrade, got {error}");
    };
    assert_eq!(response.status(), 400, "the speed is refused: {response:?}");
    let body = String::from_utf8_lossy(response.body().as_deref().unwrap_or_default()).into_owned();
    assert!(
        body.contains("speed must be finite"),
        "the refusal is the speed check rather than some other 400: {body}"
    );

    // And the incumbent is still being served afterwards. `while_draining`
    // returns the instant the refusal resolves, so a close the eviction had
    // already queued would never be read - the drain has to keep going. Read
    // frames rather than wait a while: the close rides the priority lane and
    // the writer is biased to it, so if this socket had been evicted the very
    // next frame would be that close rather than more market data.
    let alive = common::deadline(Duration::from_secs(10));
    for _ in 0..5 {
        loop {
            match tokio::time::timeout_at(alive, incumbent.next()).await {
                Err(_) => panic!("the incumbent stopped being served after the refused upgrade"),
                Ok(Some(Ok(Message::Text(_)))) => break,
                Ok(None | Some(Err(_) | Ok(Message::Close(_)))) => panic!(
                    "the venue ended the incumbent's socket on an upgrade it REFUSED, so a 400 \
                     disconnects a live consumer"
                ),
                Ok(Some(Ok(_))) => {}
            }
        }
    }

    // The positive control: the same claim, legal this time, does take the
    // account - so the survival above is the refusal sparing the incumbent
    // rather than eviction being broken or unobservable on this socket.
    let (_newcomer, _) = tokio_tungstenite::connect_async(format!(
        "{}&account=WYRD-820&callsign=beta&speed=1",
        venue.ws_url_for(&venue.symbol)
    ))
    .await
    .expect("a legal claim on an existing account is admitted");
    let deadline = common::deadline(Duration::from_secs(10));
    let evicted = loop {
        match tokio::time::timeout_at(deadline, incumbent.next()).await {
            Err(_) => break false,
            Ok(None | Some(Err(_) | Ok(Message::Close(_)))) => break true,
            Ok(Some(Ok(_))) => {}
        }
    };
    assert!(
        evicted,
        "the incumbent is evicted by a claim the venue ACCEPTS, which is what makes its survival \
         of the refused one meaningful"
    );
}

/// A socket going away frees its account all the way to collection, and the
/// thing that frees it is the attach being given up rather than the lane
/// being released.
///
/// A socket is counted onto its account before the 101 and off it when its
/// passenger is done, because the lane table alone cannot answer whether anybody
/// is reading an account: an eviction retires the incumbent's lane immediately,
/// and a newcomer binds its own only once its handler runs. The consequence
/// that matters here is one of order. `handle_socket` releases its lane while
/// still holding its attach, so the lane release finds the account still
/// counted-in and declines to freeze; the freeze is owed by the attach's own
/// departure a moment later. An account that never freezes is never
/// TTL-collected and is still swept while riding no boat.
///
/// The observable is collection, not a flag. `POST /accounts` refuses an id the
/// venue still holds a ledger for with a 409 and answers 201 once that ledger
/// has been collected, so a second open is a direct read of whether the freeze
/// ever happened. Nothing else about a frozen account is visible from outside.
///
/// The account is attached before it is abandoned, which is what stops this
/// from passing vacuously. An account is born frozen - `POST /accounts` and
/// first sight alike - so a gate that opened one and then watched it be
/// collected would be watching the birth freeze age out and would stay green
/// with every freeze in the venue disabled. Draining to a frame first proves
/// the socket bound its lane and `resume` unfroze the account, so the only way
/// back to collectable is a freeze this teardown performed.
///
/// What it still does not reach, stated because a bite-check went looking. The
/// case the attach count was added for is the upgrade abandoned before
/// `handle_socket` ever runs - no lane bound, no lane released - and no consumer
/// behaviour reaches it from outside: writing the request and resetting the
/// connection at once still loses to the venue, which has read the request,
/// written the 101 and started the handler by the time the reset lands.
/// Sixteen such attempts all landed on the handled path. That branch is pinned
/// by `run.rs`'s unit tests, which drop an `Attach` directly; pinning it
/// from a socket needs a scheduling lever the venue does not expose.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn a_departing_socket_freezes_its_account_into_collection() {
    let venue = spawn(&["--config", &account_ttl_config()]);
    let (mut socket, _) = tokio_tungstenite::connect_async(format!(
        "{}&account=WYRD-830&callsign=alpha&speed=1",
        venue.ws_url_for(&venue.symbol)
    ))
    .await
    .expect("the account is claimed");

    // An observable rather than a sleep: `connect_async` returns at the 101,
    // and the feed task that writes this frame is spawned after `bind_lanes`
    // and after `resume`. One frame therefore proves the account is attached,
    // which is the premise the assertion below rests on.
    let bound = common::deadline(Duration::from_secs(10));
    loop {
        match tokio::time::timeout_at(bound, socket.next()).await {
            Err(_) => panic!("the socket never received a frame, so it never bound its lane"),
            Ok(Some(Ok(Message::Text(_)))) => break,
            Ok(None | Some(Err(_) | Ok(Message::Close(_)))) => {
                panic!("the venue ended the socket before the test began")
            }
            Ok(Some(Ok(_))) => {}
        }
    }
    drop(socket);

    // Collected, not merely collectable: the reaper sweeps at a fraction of the
    // configured TTL, so this polls to a deadline rather than sleeping once.
    let deadline = common::wall_deadline(Duration::from_secs(5));
    let mut last = String::new();
    let reopened = loop {
        let (status, body) = http_post_json(
            &venue.http_base(),
            "/accounts",
            r#"{"account_id":"WYRD-830","balances":{"USDT":"1000"}}"#,
        );
        if status == 201 {
            break true;
        }
        last = format!("{status}: {body}");
        if std::time::Instant::now() >= deadline {
            break false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(
        reopened,
        "the account outlived the socket that was reading it: nothing froze it when that socket \
         departed, so it is never TTL-collected and is still swept while riding no boat (last \
         answer {last})"
    );
}

/// A passenger's ride ends with its socket, not when the account freezes.
///
/// Two unnamed sockets share the default account, so closing one leaves the
/// account attached and never freezes it. If the departed passenger's ride
/// survived, a reconnect on that river at a new cadence would be refused
/// against a cadence nobody is riding - and, because a boat key is just
/// (river, speed), the ledger would be handed the next boat placed there
/// whether or not it ever boarded.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn a_ride_ends_with_its_passenger_while_the_account_stays() {
    let venue = spawn(&["--config", &two_symbols_config()]);
    // The second river keeps the default account attached throughout, so
    // nothing here is explained by a freeze.
    let (_holder, _) =
        tokio_tungstenite::connect_async(format!("{}&speed=2", venue.ws_url_for("MNQ")))
            .await
            .expect("hold the account open on another river");

    let (leaving, _) =
        tokio_tungstenite::connect_async(format!("{}&speed=2", venue.ws_url_for(&venue.symbol)))
            .await
            .expect("board the river being tested");
    drop(leaving);

    // Poll: the close is asynchronous, and the ride ends as the
    // passenger unwinds rather than as the consumer's socket drops.
    let deadline = common::deadline(Duration::from_secs(10));
    loop {
        let reconnect = tokio_tungstenite::connect_async(format!(
            "{}&speed=3",
            venue.ws_url_for(&venue.symbol)
        ))
        .await;
        match reconnect {
            Ok((socket, _)) => {
                drop(socket);
                return;
            }
            Err(error) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "the ended ride still refuses a new cadence: {error}"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}

/// An account not funded in what its symbol settles in is refused at bind,
/// naming the currency.
///
/// The venue's `[balances]` is only what an unnamed account opens with, so a
/// consumer that named its own funding cannot be checked at boot - the venue has
/// no way to know then what it will say. It is still knowable with no order at
/// all, though, so it stays a configuration error rather than becoming a
/// fill-time funds rejection: collapsing the two would make a typo look like a
/// trading outcome and waste a whole run.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn an_account_funded_in_the_wrong_currency_is_refused_at_bind() {
    let venue = spawn(&["--config", &fast_config()]);
    let (status, body) = http_post_json(
        &venue.http_base(),
        "/accounts",
        r#"{"account_id":"WYRD-600","balances":{"JPY":"5000000"}}"#,
    );
    assert_eq!(
        status, 201,
        "the account opens on whatever it named: {body}"
    );

    // Through a real upgrade attempt: the refusal is a status before the 101,
    // and a plain GET never reaches the handler because the upgrade extractor
    // rejects it first.
    let refused = tokio_tungstenite::connect_async(format!(
        "{}?account=WYRD-600&symbol={}",
        venue.ws_url(),
        venue.symbol
    ))
    .await;
    let Err(error) = refused else {
        panic!("the bind is refused, not served");
    };
    // `contains("400") || contains("HTTP")` was close to unfalsifiable: the
    // second arm matches the Display of essentially every tungstenite error,
    // a connection refusal and a 500 included, and the docstring's own claim -
    // that the refusal names the currency - was never asserted at all. Both are
    // read off the structured response now.
    let tokio_tungstenite::tungstenite::Error::Http(response) = error else {
        panic!("the refusal is an HTTP status before the upgrade, got {error}");
    };
    assert_eq!(
        response.status(),
        400,
        "the refusal is a status before the 101: {response:?}"
    );
    let body = String::from_utf8_lossy(response.body().as_deref().unwrap_or_default()).into_owned();
    assert!(
        body.contains("WYRD-600") && body.contains("not funded in"),
        "the refusal is the funding check rather than some other 400: {body}"
    );
    // USDT as a literal, not resolved through the same config code the venue
    // runs: this is the settlement currency of the default boot river, and a
    // derived expectation would compare the venue's answer to itself.
    //
    // The whole phrase, not `contains("USDT")`. The boot river here is the
    // default preset BTCUSDT, and "USDT" is a substring of "BTCUSDT" - so a bare
    // `contains("USDT")` was implied by the symbol assertion beside it and said
    // nothing on its own. A venue that had regressed to echoing the account's
    // own currency back - "not funded in JPY, which is what BTCUSDT settles in",
    // a plausible real bug - would have satisfied it.
    assert!(
        body.contains("not funded in USDT"),
        "the refusal names the settlement currency, not the account's own: {body}"
    );
    assert!(
        body.contains(&*venue.symbol),
        "the refusal names the river whose settlement it is: {body}"
    );
}

/// A perpetual position pays funding, which is the only thing tying a perp to
/// spot when it has no expiry to converge at.
///
/// Without it a strategy holding a perp across funding instants has forward P
/// and L that is wrong by construction rather than by approximation, so this is
/// a correctness gate rather than a fidelity nicety.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn a_perpetual_position_pays_funding_across_an_interval() {
    let venue = spawn(&["--config", &perpetual_config()]);
    let (mut socket, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("open the perpetual socket");

    let submit = format!(
        r#"{{"type":"SubmitOrder","client_order_id":"PERP-1","symbol":"{}","side":"Buy","order_type":"Market","quantity":"5","time_in_force":"Gtc"}}"#,
        venue.symbol
    );
    socket
        .send(Message::Text(submit.into()))
        .await
        .expect("open a long perpetual position");

    // The condition this test needs is "the position is open", and the venue
    // states it on the wire. Waiting a fixed 500 ms for it instead both bet on
    // the host and left the socket unread on an unpaced tape, where an unread
    // socket is eventually ejected by the bounded fanout ring - and the funding
    // assertion below would then have been read as funding not being charged.
    let deadline = common::deadline(common::TEST_WALL_BUDGET);
    loop {
        let message = tokio::time::timeout_at(deadline, socket.next())
            .await
            .expect("the perpetual position opens within 30 s")
            .expect("the venue CLOSED the perpetual socket before the position opened")
            .expect("a well-formed frame");
        if let Message::Text(text) = message
            && let Ok(VenueMessage::OrderFilled(fill)) = serde_json::from_str::<VenueMessage>(&text)
            && fill.client_order_id == "PERP-1"
        {
            break;
        }
    }
    // Nothing else on this socket is read, but it must keep being drained for
    // the rest of the run for the same reason - and the drain remembers how the
    // stream ended, so an eviction during the funding wait is named as one
    // instead of surfacing as an unmoved balance.
    let drain = BackgroundDrain::spawn(socket);

    let balance_of = |body: &str| -> f64 {
        let value: serde_json::Value = serde_json::from_str(body).unwrap();
        value["balances"]
            .as_array()
            .expect("balances")
            .iter()
            .find(|row| row["currency"] == "USDT")
            .and_then(|row| row["total"].as_str())
            .expect("a USDT total")
            .parse()
            .expect("a decimal total")
    };
    let (_, before) = http_get(&venue.http_base(), "/account");
    let before = balance_of(&before);

    // Several sweep passes, each crossing at least one one-second funding
    // instant on this venue's clock.
    //
    // This one stays a wall sleep, and the reason is worth writing down because
    // the obvious replacement was tried and is vacuous here. The binding
    // resource is the sweeper, which runs on a wall cadence; funding is charged
    // by a sweep pass, not by the clock reaching an instant. And this config is
    // `speed = 0.0`, where the two axes come apart in a way that defeats the
    // obvious poll: the boat's clock is still built wall-rated (a zero speed is
    // replaced by 1.0 when the `SimClock` is constructed), while delivery is
    // unpaced, so the tape's `ts_event` runs far ahead of `venue_now_ns`.
    // Anchoring a clock target on a tape stamp therefore satisfies it at once
    // and the test fails on an unmoved balance, which is what was measured.
    // Nothing the venue serves
    // counts sweep passes, so there is no condition to wait on: a monotonic
    // per-river count of completed passes on `/clock` or `/health` is what would
    // turn this sleep into a condition, and nothing on the wire carries it.
    tokio::time::sleep(Duration::from_millis(3_000)).await;
    let (_, after) = http_get(&venue.http_base(), "/account");
    let after = balance_of(&after);
    drain.stop("the perpetual socket").await;

    assert!(
        after < before,
        "a long perpetual must pay funding across an interval: {before} -> {after}"
    );
}

/// Two sockets that name no account both live. Only a claimed account evicts.
///
/// This is the shape the default account exists to serve, and the eviction
/// landing broke it: both sockets resolve to the default, so keying eviction on
/// the account alone made the second close the first - a consumer evicting itself
/// by opening a second socket. Naming an id is a statement about identity and
/// eviction is the answer to it; naming none is the consumer saying it has no
/// opinion.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn two_sockets_naming_no_account_both_stay_open() {
    let venue = spawn(&["--config", &fast_config()]);
    let (mut first, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("open the first socket");
    let (mut second, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("open the second socket");

    // Both are still being served: neither closed, and both keep receiving.
    // One shared deadline rather than a fresh 10 s per socket, which summed to
    // the whole per-test watchdog and would have been killed rather than
    // reported.
    let deadline = common::deadline(Duration::from_secs(10));
    for (label, socket) in [("first", &mut first), ("second", &mut second)] {
        let seen = tokio::time::timeout_at(deadline, socket.next()).await;
        assert!(
            matches!(seen, Ok(Some(Ok(Message::Text(_))))),
            "the {label} socket stopped being served: {seen:?}"
        );
    }
}

/// An unpoliced account is enforced against nothing, which is what every consumer
/// had before policies existed and what the default account still gets.
#[test]
#[ignore = "binds a loopback listener"]
fn an_account_naming_no_policy_is_unpoliced() {
    let venue = spawn(&["--config", &fast_config()]);
    let (status, body) = http_get(&venue.http_base(), "/account");
    assert_eq!(status, 200, "the default account answers: {body}");
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    let risk = &value["risk"];
    assert!(
        risk["trailing_threshold"].is_null() && risk["daily_remaining"].is_null(),
        "an unpoliced account states no thresholds: {body}"
    );
    assert!(risk["breached"].is_null(), "nothing to breach: {body}");
}

/// The 2026-08-02 defect, stated positively. A divergence armed over the
/// control plane used to be diverted onto an auto-created account slot and
/// never reached the market-data socket. With one run there is no other slot to
/// divert to, so it must reach every connection.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn an_armed_divergence_reaches_every_connection() {
    let venue = spawn(&["--config", &paced_config()]);
    let (mut data_socket, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("open the market-data socket");

    // Prove the feed is live before arming, so a later silence is the
    // divergence rather than a socket that never worked.
    let deadline = common::deadline(common::TEST_WALL_BUDGET);
    loop {
        let message = tokio::time::timeout_at(deadline, data_socket.next())
            .await
            .expect("the tape flows before the blackout")
            .expect("open")
            .expect("frame");
        if matches!(message, Message::Text(ref text)
            if matches!(serde_json::from_str(text), Ok(VenueMessage::Trade(_))))
        {
            break;
        }
    }

    // Arm a blackout over the control plane. It is armed against the run, not
    // against an account, so it must gate this socket's market data.
    let armed = post_divergence(&venue.http_base(), r#"{"type":"StallData","ms":180000}"#);
    assert_eq!(armed, 202, "the divergence is accepted");
    // No ceiling, and no clock read. This used to read `/clock?symbol=` for the
    // boat's own published instant and assert that nothing arriving was stamped
    // past it. That route no longer answers on a boat - a per-boat clock on an
    // anonymous endpoint was a boat-discovery channel - and the venue clock
    // cannot stand in for it: it runs ahead of what a boat has published, so it
    // is a generous ceiling that post-arm water can pass under.
    //
    // What replaces it is stronger rather than weaker. A blackout is gated at
    // send time, so the venue chooses to write nothing at all once the arm
    // lands, and the honest assertion is silence rather than an ordering bound.
    // The only frames that may still arrive are bytes already on the wire when
    // the ack came back, so they are drained explicitly and named as such
    // instead of being admitted by a comparison.
    let settle_until = tokio::time::Instant::now() + Duration::from_millis(250);
    while let Ok(Some(Ok(_))) = tokio::time::timeout_at(settle_until, data_socket.next()).await {}

    // Within the window no market data may arrive on this socket.
    //
    // Deliberately not budget-clamped, unlike every deadline above it. This is
    // not a bound on how long the test may wait for something, it is the length
    // of the observation the property is asserted over: shortening it does not
    // make the test fail sooner, it makes the test pass on less evidence. Two
    // seconds is what the arming above was sized against.
    let quiet_until = tokio::time::Instant::now() + Duration::from_secs(2);
    while let Ok(Some(Ok(message))) = tokio::time::timeout_at(quiet_until, data_socket.next()).await
    {
        if let Message::Text(text) = message {
            let event_ts = match serde_json::from_str::<VenueMessage>(&text) {
                Ok(VenueMessage::Trade(trade)) => Some(trade.ts_event),
                Ok(VenueMessage::Quote(quote)) => Some(quote.ts_event),
                _ => None,
            };
            assert!(
                event_ts.is_none(),
                "market data arrived inside an armed StallData window, stamped \
                 {event_ts:?}; the venue gates at send time, so it should have \
                 written nothing at all"
            );
        }
    }
}

/// The run owns the fill sweep now that no account does. Without a sweep task
/// on the run, a banded limit rests forever: a submit decides only its own
/// order, against the reading it arrived with, so nothing else ever walks the
/// span its trigger waits on, and the venue would be accepting orders it can
/// never execute.
///
/// Its premise is not enforced by construction and is asserted rather than
/// assumed: the limit is placed 2.01 below the last historical print, and if
/// the market falls that far between reading the anchor and the submit landing
/// the order is marketable on arrival and never rests at all. That is a lost
/// bet on sim-time drift, not a defect in the sweep, and the liquidity side on
/// the fill is what tells the two apart.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn a_banded_limit_fills_from_the_run_sweep() {
    let venue = spawn(&["--config", &band_config()]);
    let (mut socket, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("open the order socket");

    let (_, clock_body) = http_get(&venue.http_base(), "/clock");
    let clock: serde_json::Value = serde_json::from_str(&clock_body).expect("clock");
    let sim_now = clock["venue_now_ns"].as_u64().expect("sim now");
    let (_, trades_body) = http_get(
        &venue.http_base(),
        &format!(
            "/operator/trades?symbol={}&start={}&end={sim_now}&limit=10000",
            venue.symbol,
            sim_now.saturating_sub(300_000_000_000)
        ),
    );
    let trades: Vec<TradeTick> = serde_json::from_str(&trades_body).expect("anchor trades");
    let price = trades.last().expect("anchor print").price - rust_decimal::Decimal::new(201, 2);
    let submit = format!(
        r#"{{"type":"SubmitOrder","client_order_id":"BAND-1","symbol":"{}","side":"Buy","order_type":"Limit","quantity":"0.01","price":"{price}","time_in_force":"Gtc"}}"#,
        venue.symbol,
    );
    socket
        .send(Message::Text(submit.into()))
        .await
        .expect("submit the gated limit");

    let deadline = common::deadline(common::TEST_WALL_BUDGET);
    let mut accepted_ts = None;
    loop {
        let message = tokio::time::timeout_at(deadline, socket.next())
            .await
            .expect("the sweep filled the resting limit within the tape's dwell bound")
            .expect("the socket stays open")
            .expect("a well-formed frame");
        let Message::Text(text) = message else {
            continue;
        };
        match serde_json::from_str::<VenueMessage>(&text) {
            Ok(VenueMessage::OrderAccepted { ts_event, .. }) => accepted_ts = Some(ts_event),
            Ok(VenueMessage::OrderFilled(fill)) => {
                assert_eq!(fill.client_order_id, "BAND-1");
                // The liquidity side, not the timestamp, is what names the case.
                // A swept fill is a Maker fill; a limit that was already
                // marketable when the submit landed fills as a Taker in the
                // accept's own engine batch. The premise this test rests on is
                // that the 2.01 of headroom read off the anchor print survives
                // the sim time between reading it and the submit landing, and at
                // speed 100 a small wall shift is a large sim shift - so the
                // premise really does lose sometimes. When it does, this says
                // the order never rested, which is the truth, instead of blaming
                // publication order and sending a reader hunting a serving
                // defect that is not there.
                assert_eq!(
                    fill.liquidity_side,
                    LiquiditySide::Maker,
                    "the limit was marketable on arrival and took liquidity, so \
                     it never rested for the sweep to find - the market moved \
                     more than the 2.01 of headroom between the anchor read and \
                     the submit, not a sweep or ordering defect"
                );
                assert!(
                    accepted_ts.is_some_and(|accepted| fill.ts_event > accepted),
                    "a banded limit must rest before the sweep fills it"
                );
                return;
            }
            Ok(VenueMessage::OrderRejected { reason, .. }) => {
                panic!("the banded limit was rejected: {reason}")
            }
            // Named rather than swallowed by a wildcard: each of these three
            // says the venue never got a chance to do what the test is asking
            // about, and under a wildcard the test would instead die on the
            // 60-second deadline blaming the sweep.
            Ok(VenueMessage::AdmissionRejected {
                subject, reason, ..
            }) => panic!("the venue refused admission for {subject:?}: {reason}"),
            Ok(VenueMessage::ProtocolError { reason, .. }) => {
                panic!("the venue read the submit as malformed: {reason}")
            }
            // Everything else is ignored, `FeedLagged` included. A declared
            // market-view hole says nothing about the fill: order events ride
            // the held lane, which is unbounded and pumped into the writer, and
            // only the market ring overruns. This used to panic on it, on the
            // assumption the two shared a fate.
            _ => {}
        }
    }
}

/// Market slippage is only real if the submit path actually takes a reading, and
/// it has to take one on BOTH market paths - the priced one and the price-less
/// one that used to return early with a stamped price and no reading at all. An
/// engine that slips perfectly would otherwise never receive a reading in
/// production while every engine-side unit test passed.
///
/// The fill price cannot prove it on the price-less arm, which is why this test
/// reads the venue's log. On the priced arm the price is evidence: the order
/// states an absurd 9000000, the no-reading fallback fills at that stated price,
/// and a fill near the tape is therefore only possible if the venue read. On the
/// price-less arm there is no stated price to be absurd - `market_reading`
/// stamps the order with the last print either way, from the reading when there
/// is one and from `fills::read_last` when there is not, and the engine's
/// no-reading branch then fills at that same stamp. Both outcomes land on the
/// tape, so `fill.last_px < last * 2` is satisfied by a fill decided off no
/// reading at all - it was green by construction for the very arm the docstring
/// says this test exists for.
///
/// The one observable that separates them is the engine's own `warn`, `market
/// order has no market reading; using its stated price`, emitted with the
/// client order id. An attempt whose id never appears in it took a reading.
/// The two evidences are cross-checked against each other on the priced arm,
/// so neither the log nor the wire is trusted alone.
///
/// Retried, because `read_market` legitimately refuses at any instant whose
/// trailing window carries fewer than `MIN_VOL_SAMPLES` returns, and the fitted
/// BTCUSDT tape does that at a substantial fraction of instants. A refused
/// reading is the documented fallback - stated price, no slippage, warn - so a
/// single attempt would be a coin flip. What is pinned is that a reading is
/// taken on both paths: every attempt warning would mean the path never reads at
/// all, which is the defect this test exists for.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn a_market_submit_takes_a_reading_on_both_the_priced_and_priceless_paths() {
    const ATTEMPTS: usize = 8;
    /// The engine's no-reading fallback, verbatim from `orders.rs`.
    const NO_READING: &str = "market order has no market reading";
    let venue = spawn(&["--config", &band_config()]);
    let log = &venue.log;
    // Before anything is concluded from an absence in that buffer. The property
    // below is scored on attempts whose id does not appear beside the warn, and
    // an empty buffer satisfies that for every attempt - a silenced filter, a
    // dead capture thread or a closed pipe would all render as "the venue always
    // took a reading". `common::spawn` pins `RUST_LOG` so the ambient
    // environment cannot silence it, and this proves the pin took effect and the
    // lines actually arrive here.
    log.await_positive_control();
    let (mut socket, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("open the order socket");

    for path in ["priced", "priceless"] {
        let price = if path == "priced" {
            r#","price":"9000000""#
        } else {
            ""
        };
        // (id, whether the fill landed on the tape rather than at 9000000).
        let mut attempts: Vec<(String, bool)> = Vec::new();
        for attempt in 0..ATTEMPTS {
            let id = format!("MKT-{path}-{attempt}");
            // The lower end of the bracket the floor below is taken over: the
            // last market instant this socket has actually been given before the
            // submit goes out. Delivery is monotone and the handler reads a tape
            // at or beyond what it has already published, so this is a true
            // lower bound. Anchoring the window on the acceptance instant
            // instead, which is what this did, put the window's start after
            // prints the reading could legitimately have been taken at, and the
            // floor then rejected honest fills.
            //
            // Not `/clock`, and not because the read is expensive: that route is
            // a run fact now and the run clock runs ahead of what any boat has
            // published, so it is not a lower bound at all and would reinstate
            // exactly the too-late anchor described above. What the socket has
            // been handed is its own knowledge and needs no route.
            let before_ns = drain_last_market_ts(&mut socket).await;
            let submit = format!(
                r#"{{"type":"SubmitOrder","client_order_id":"{id}","symbol":"{}","side":"Buy","order_type":"Market","quantity":"0.01"{price},"time_in_force":"Gtc"}}"#,
                venue.symbol
            );
            socket
                .send(Message::Text(submit.into()))
                .await
                .expect("submit the market order");

            let deadline = common::deadline(common::TEST_WALL_BUDGET);
            let mut accepted_ts = None;
            let fill = loop {
                let message = tokio::time::timeout_at(deadline, socket.next())
                    .await
                    .expect("the venue answered the market submit")
                    .expect("the socket stayed open")
                    .expect("a websocket frame");
                let Message::Text(text) = message else {
                    continue;
                };
                match serde_json::from_str::<VenueMessage>(&text) {
                    Ok(VenueMessage::OrderAccepted {
                        client_order_id,
                        ts_event,
                        ..
                    }) if client_order_id == id => accepted_ts = Some(ts_event),
                    Ok(VenueMessage::OrderFilled(fill)) if fill.client_order_id == id => {
                        break fill;
                    }
                    Ok(VenueMessage::OrderRejected { reason, .. }) => {
                        panic!("{id} was rejected: {reason}")
                    }
                    _ => {}
                }
            };

            // The venue decides a market submit against `MarketReadingCache`,
            // which memoizes `read_market` on the sweep-interval bucket (10 ms
            // under band.toml). The reading therefore names the last print at or
            // before the start of the acceptance instant's bucket, and the
            // adverse-slippage invariant has to be asserted against that print,
            // not against the last print at the fill instant - the tape moves on
            // between the reading and the fill, and a buy filling below a print
            // it could not have seen is not favourable slippage.
            //
            // Worse, the reading instant is not the acceptance instant either:
            // it is whenever the submit reached the handler, which at speed 100
            // can be many sim seconds earlier. So the print the venue read
            // cannot be identified from outside - only bracketed. What is
            // asserted is therefore the strongest statement that survives the
            // bracket: the reading's print lies somewhere in the lookback below,
            // and the fill band is adverse, so a market buy must fill at or
            // above the lowest price in that window. Recovering the exact
            // per-fill statement means putting the reading instant on the
            // `OrderFilled` event or dropping the cache's bucketing; neither is
            // this test's call to make.
            const BUCKET_NS: u64 = 10_000_000;
            let reading_ts = accepted_ts.unwrap_or(fill.ts_event) / BUCKET_NS * BUCKET_NS;
            // A 60 s lookback below the pre-submit instant, not below the
            // acceptance one, so the window covers every print the reading could
            // have named. Not 1 s, because the arrival clock's quiet state runs a
            // mean gap of several seconds and a one-second window is legitimately
            // empty often enough to make this flaky for a reason that has nothing
            // to do with fills.
            //
            // Fetched through `trade_window`, which pages. A single `/trades`
            // query returns the window's oldest prints when the page fills, and
            // the floor was then taken over stale water the market had since
            // fallen through - asserted as favourable slippage that never
            // happened. Guarding that with `trades.len() < PAGE` instead would
            // have turned a slow round trip, which widens this window without
            // bound at speed 100, into a red test on a run where nothing is
            // wrong.
            let trades = trade_window(
                &venue.http_base(),
                &venue.symbol,
                before_ns.saturating_sub(60_000_000_000),
                reading_ts,
            );
            let last = trades
                .last()
                .expect("a print at or before the reading")
                .price;
            let floor = trades
                .iter()
                .map(|trade| trade.price)
                .min()
                .expect("a print in the lookback");
            let on_the_tape = fill.last_px < last * rust_decimal::Decimal::TWO;
            if on_the_tape {
                assert!(
                    fill.last_px >= floor,
                    "a market buy filled below every print it could have read: {} < {floor}",
                    fill.last_px
                );
            }
            attempts.push((id, on_the_tape));
            // Let the clock move so the next attempt reads a genuinely different
            // window - at speed 100 this is fifty sim seconds of fresh tape.
            // Unconditional now: every attempt is scored, so there is no early
            // exit to skip it. The count and this gap are the whole flake margin
            // on the assertion below, whose false-failure mode is every attempt
            // legitimately refusing, so neither may be trimmed for wall time
            // without something else compensating.
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        // The venue writes the warn before it emits the fill, but that is its
        // stderr pipe and this is a websocket, so the draining thread is allowed
        // a settle. Bounded because the question is an absence, which nothing
        // can signal; the loop's own last inter-attempt sleep already gave it
        // 500 ms, and this adds a second one rather than betting on a single.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let warned: Vec<&(String, bool)> = attempts
            .iter()
            .filter(|(id, _)| log.contains_both(NO_READING, id))
            .collect();
        assert!(
            warned.len() < attempts.len(),
            "the {path} market path warned `{NO_READING}` on all {} attempts, so it never took \
             a reading at all",
            attempts.len()
        );
        // The log and the wire must agree, which is what keeps this from being
        // a test of the log alone. On the priced arm the two are independently
        // observable: an attempt that did not warn took a reading, and a fill
        // that took a reading cannot have landed on the order's own absurd
        // 9000000. If they disagree, one of the two observables is lying and
        // the other findings resting on either are worthless.
        if path == "priced" {
            for (id, on_the_tape) in &attempts {
                let warned = warned.iter().any(|(warned_id, _)| warned_id == id);
                assert_eq!(
                    !warned,
                    *on_the_tape,
                    "{id}: the venue's log says a reading was {} while its fill says the opposite",
                    if warned { "NOT taken" } else { "taken" }
                );
            }
        }
    }
}

#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn the_tape_is_identical_with_and_without_order_flow() {
    let venue = spawn(&["--config", &band_config()]);
    let path = format!(
        "/operator/trades?symbol={}&start={}&limit=200",
        venue.symbol, venue.record.data_origin_ns
    );
    let (status, before) = http_get(&venue.http_base(), &path);
    assert_eq!(status, 200);
    let (socket, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("open order socket");
    // The socket is split and drained while the submits go out. Pushing a
    // hundred submits with nothing reading left the socket unread for the whole
    // burst, and this venue's fanout is a bounded ring: an unread socket on a
    // live tape loses market frames and is told so, which the drain below then
    // reported as if the tape-purity property had broken.
    let (mut writer, mut reader) = socket.split();
    let submitting = async {
        for index in 0..100 {
            let submit = format!(
                r#"{{"type":"SubmitOrder","client_order_id":"TAPE-{index}","symbol":"{}","side":"Buy","order_type":"Limit","quantity":"0.01","price":"1","time_in_force":"Gtc"}}"#,
                venue.symbol
            );
            writer
                .send(Message::Text(submit.into()))
                .await
                .map_err(|err| format!("submit TAPE-{index} could not be sent: {err}"))?;
        }
        Ok::<_, String>(())
    };
    // Drain to the last acceptance before re-reading. Comparing the pages while
    // the submits were still in flight would let a clean run and a broken one
    // look alike.
    let draining = async {
        let deadline = common::deadline(common::TEST_WALL_BUDGET);
        loop {
            let message = tokio::time::timeout_at(deadline, reader.next())
                .await
                .map_err(|_| "the venue did not accept TAPE-99 within 60 s".to_string())?
                .ok_or_else(|| {
                    "the venue CLOSED the order socket mid-burst, so this run says nothing about \
                     tape purity"
                        .to_string()
                })?
                .map_err(|err| format!("the order socket failed in transport: {err}"))?;
            let Message::Text(text) = message else {
                continue;
            };
            if let Ok(VenueMessage::OrderAccepted {
                client_order_id, ..
            }) = serde_json::from_str::<VenueMessage>(&text)
                && client_order_id == "TAPE-99"
            {
                return Ok::<_, String>(());
            }
        }
    };
    let (submitted, drained) = tokio::join!(submitting, draining);
    submitted.unwrap_or_else(|why| panic!("{why}"));
    drained.unwrap_or_else(|why| panic!("{why}"));
    let (status, after) = http_get(&venue.http_base(), &path);
    assert_eq!(status, 200);
    assert_eq!(
        before, after,
        "consumer order flow advanced or altered the clean tape"
    );
}

/// The tape-purity property, extended to conditionals: no consumer conditional
/// advances any generator state.
///
/// A resting conditional is the one order shape that puts a second kind of scan
/// into the sweeper's per-symbol walk (`ScanKind::TriggerTouch` beside the
/// limits' `FillThrough`), and the walk drains the tape source. If a trigger
/// scan drained prints the canonical `/trades` page would otherwise have served,
/// or advanced the generator past them, the two reads of the same fixed window
/// would differ. They must not.
///
/// It lives at the venue layer rather than in `mogwai-engine`, where the spec's
/// gate list names it: the engine holds no tape and no generator, so the
/// property it asserts is only expressible where the walk and the source
/// actually are. Its twin above is the same assertion for plain limits.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn the_tape_is_identical_with_and_without_a_resting_stop() {
    let venue = spawn(&["--config", &band_config()]);
    let path = format!(
        "/operator/trades?symbol={}&start={}&limit=200",
        venue.symbol, venue.record.data_origin_ns
    );
    let (status, before) = http_get(&venue.http_base(), &path);
    assert_eq!(status, 200);
    let (socket, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("open order socket");
    // Split and drained while sending, for the same reason as the twin above: a
    // socket left unread through a hundred submits on a live tape is ejected by
    // the bounded fanout ring, and the drain would report that eviction as a
    // tape-purity failure.
    let (mut writer, mut reader) = socket.split();
    // Sell stops at a trigger of 1: unreachable by any BTCUSDT print, so every
    // one of them remains resting and untriggered for the whole test, which is
    // the state that puts a touch scan into every sweep pass. Half stop-market
    // and half stop-limit, because the two rest identically as
    // `Resting::Conditional` but reach the walk through different submit paths.
    // They are `reduce_only`, which is what a protective leg on a flat book
    // actually is: the funded account holds no BTC, and section 1.8's admission
    // exemption is precisely what lets such a leg rest rather than be refused at
    // the door.
    let submitting = async {
        for index in 0..100 {
            let submit = if index % 2 == 0 {
                format!(
                    r#"{{"type":"SubmitOrder","client_order_id":"STOP-{index}","symbol":"{}","side":"Sell","order_type":"StopMarket","quantity":"0.01","trigger_price":"1","reduce_only":true,"time_in_force":"Gtc"}}"#,
                    venue.symbol
                )
            } else {
                format!(
                    r#"{{"type":"SubmitOrder","client_order_id":"STOP-{index}","symbol":"{}","side":"Sell","order_type":"StopLimit","quantity":"0.01","price":"1","trigger_price":"1","reduce_only":true,"time_in_force":"Gtc"}}"#,
                    venue.symbol
                )
            };
            writer
                .send(Message::Text(submit.into()))
                .await
                .map_err(|err| format!("submit STOP-{index} could not be sent: {err}"))?;
        }
        Ok::<_, String>(())
    };
    let draining = async {
        let deadline = common::deadline(common::TEST_WALL_BUDGET);
        loop {
            let message = tokio::time::timeout_at(deadline, reader.next())
                .await
                .map_err(|_| "the venue did not accept STOP-99 within 60 s".to_string())?
                .ok_or_else(|| {
                    "the venue CLOSED the order socket mid-burst, so this run says nothing about \
                     tape purity"
                        .to_string()
                })?
                .map_err(|err| format!("the order socket failed in transport: {err}"))?;
            let Message::Text(text) = message else {
                continue;
            };
            match serde_json::from_str::<VenueMessage>(&text) {
                Ok(VenueMessage::OrderRejected {
                    client_order_id,
                    reason,
                    ..
                }) => panic!("{client_order_id} was rejected: {reason}"),
                Ok(VenueMessage::OrderTriggered {
                    client_order_id, ..
                }) => panic!("{client_order_id} triggered: a trigger of 1 is unreachable"),
                Ok(VenueMessage::OrderAccepted {
                    client_order_id, ..
                }) if client_order_id == "STOP-99" => return Ok::<_, String>(()),
                _ => {}
            }
        }
    };
    let (submitted, drained) = tokio::join!(submitting, draining);
    submitted.unwrap_or_else(|why| panic!("{why}"));
    drained.unwrap_or_else(|why| panic!("{why}"));
    // Let several sweep passes run with the hundred touch scans in the book.
    // Draining to the last acceptance only proves the submit path is pure; the
    // walk is what this test is actually about, and it runs on its own cadence
    // (`fill_sweep_interval_ms = 10`).
    //
    // This waits on the venue's own clock rather than on a wall sleep. Nothing
    // the venue serves counts sweep passes, so no observable here can prove one
    // ran; what a clock poll does establish is that the boat is alive and its
    // sim axis has moved far past the last acceptance, which a fixed sleep does
    // not - a stalled venue would have satisfied the sleep and been read as a
    // clean pass. At speed 100 the fifty sim-seconds below are about half a wall
    // second, so fifty sweep opportunities at the 10 ms cadence, and the poll
    // waits longer if the venue is slow instead of proceeding early.
    //
    // `reader` keeps being drained across that wait. It is the only socket on
    // this venue and it is attached to the live tape at speed 100, so parking it
    // for the half wall second below reopens the very window the split was
    // introduced to close - and the eviction would not even surface as a tape
    // failure: the clock poll would keep polling, and if the boat went with the
    // connection `venue_sim_now` would panic about a river carrying no boat.
    let drain = BackgroundDrain::spawn(reader);
    let sim_at_acceptance = venue_sim_now(&venue.http_base(), "/clock");
    let target = sim_at_acceptance + 50_000_000_000;
    let deadline = common::deadline(common::TEST_WALL_BUDGET);
    while venue_sim_now(&venue.http_base(), "/clock") < target {
        drain.assert_still_serving("the order socket");
        assert!(
            tokio::time::Instant::now() < deadline,
            "the run clock did not advance 50 sim-seconds in 60 s of wall time, so no sweep \
             pass can be claimed to have run over the resting conditionals"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    drain.stop("the order socket").await;

    let (status, after) = http_get(&venue.http_base(), &path);
    assert_eq!(status, 200);
    assert_eq!(
        before, after,
        "a resting conditional's touch scan advanced or altered the clean tape"
    );
}

/// A `/ws` socket as `tokio_tungstenite` hands it back from a connect.
type WsSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// A background drain that remembers how the stream ended.
///
/// The naive shape, a spawned `while s.next().await.is_some() {}`, ends
/// identically on a close frame, a transport error and end of stream,
/// and since the handle is only ever aborted, nothing ever reads that. A venue
/// that ejects the socket mid-wait is then invisible, and whatever the test
/// asserts afterwards fails as if the property had broken. That is the exact
/// misdiagnosis this file spent a pass removing from the foreground drains, so
/// the background ones may not reintroduce it: this records the ending and
/// `assert_still_serving` is what turns it back into a truthful message.
struct BackgroundDrain {
    handle: tokio::task::JoinHandle<()>,
    ended: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

impl BackgroundDrain {
    /// Starts draining `stream` until it ends or the drain is stopped.
    fn spawn<S>(mut stream: S) -> Self
    where
        S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
            + Unpin
            + Send
            + 'static,
    {
        let ended = std::sync::Arc::new(std::sync::Mutex::new(None));
        let slot = std::sync::Arc::clone(&ended);
        let handle = tokio::spawn(async move {
            let why = loop {
                match stream.next().await {
                    Some(Ok(Message::Close(frame))) => {
                        break format!("the venue sent a close frame: {frame:?}");
                    }
                    Some(Ok(_)) => continue,
                    Some(Err(err)) => break format!("the socket failed in transport: {err}"),
                    None => break "the stream ended".to_string(),
                }
            };
            *slot.lock().expect("the drain's ending is recorded") = Some(why);
        });
        Self { handle, ended }
    }

    /// Panics naming `subject` if the venue ended the stream while it drained.
    /// Call this before asserting the property the wait was for.
    ///
    /// It reads a record another task writes, so a silent return means "nothing
    /// recorded yet" rather than "the stream is alive" - the drain runs on its
    /// own task and only writes after it has been polled. That is why [`stop`]
    /// yields first and why the fixture below yields a hundred times before
    /// checking. Every call site here sits downstream of a real await, so the
    /// drain has run; a call placed after a stretch of blocking work on the
    /// runtime thread would be reading a record nothing had a chance to write.
    ///
    /// [`stop`]: BackgroundDrain::stop
    fn assert_still_serving(&self, subject: &str) {
        if let Some(why) = self
            .ended
            .lock()
            .expect("the drain's ending is readable")
            .as_ref()
        {
            panic!(
                "the venue ended {subject} while the test waited, so this run says nothing about \
                 the property under test: {why}"
            );
        }
    }

    /// Checks the ending one last time, then stops draining.
    ///
    /// It yields before it checks, and the yield is the whole difference between
    /// a guard and a decoration. The last thing before a `stop` is often a
    /// blocking `http_get`, which holds the runtime thread, so a close frame
    /// that arrived during it is sitting unpolled: checking straight through
    /// would report "still serving" on exactly the branch this exists to catch,
    /// and the caller's assertion would then blame the property. Best-effort
    /// rather than proof - nothing can prove a socket is alive - but it costs one
    /// scheduler turn and removes the reachable half.
    async fn stop(self, subject: &str) {
        tokio::task::yield_now().await;
        self.assert_still_serving(subject);
        self.handle.abort();
    }
}

/// Runs `work` while `idle` is drained, and panics if the venue ends `idle`
/// first.
///
/// A socket the test is not reading yet is not exempt from the bounded fanout
/// ring: every socket is attached to the live tape on upgrade, so on an unpaced
/// tape an unread one is exactly what gets ejected - and the subsequent
/// `expect("the socket stays open")` would report that as the property failing.
async fn while_draining<S, F>(idle: &mut S, work: F, subject: &str) -> F::Output
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
    F: std::future::Future,
{
    tokio::pin!(work);
    loop {
        tokio::select! {
            done = &mut work => return done,
            frame = idle.next() => match frame {
                Some(Ok(Message::Close(spec))) => {
                    panic!("the venue CLOSED {subject} while the test waited: {spec:?}")
                }
                Some(Err(err)) => {
                    panic!("{subject} failed in transport while the test waited: {err}")
                }
                None => panic!("the venue ended {subject}'s stream while the test waited"),
                Some(Ok(_)) => {}
            },
        }
    }
}

/// The two drain helpers above are what every socket-backed test in this file
/// now relies on to tell a close from the property under test, so they are
/// pinned directly rather than only through the venues that use them. No
/// listener is bound: the streams are fabricated, which is the point - a venue
/// cannot be made to evict on command, and the behaviour being pinned is the
/// helper's, not the venue's.
#[tokio::test]
#[should_panic(expected = "the venue sent a close frame")]
async fn a_background_drain_names_a_close_instead_of_swallowing_it() {
    let drain = BackgroundDrain::spawn(futures_util::stream::iter(vec![
        Ok(Message::Text("a print".into())),
        Ok(Message::Close(None)),
    ]));
    // The drain runs on its own task, so the ending is observed on the next
    // yield rather than instantly - which is exactly the shape at every call
    // site, where the check sits inside a polling loop.
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    drain.assert_still_serving("the fabricated socket");
}

/// `stop`'s own yield, pinned. Nothing awaits between the spawn and the stop
/// here, which is the shape at every call site: the last thing before a `stop`
/// is a blocking `http_get`, so a close that arrived during it is sitting
/// unpolled and a check placed straight through reports "still serving" on
/// precisely the branch this machinery exists to catch.
#[tokio::test]
#[should_panic(expected = "the venue sent a close frame")]
async fn stopping_a_drain_sees_a_close_that_no_await_gave_it_a_chance_to_record() {
    let drain = BackgroundDrain::spawn(futures_util::stream::iter(vec![Ok(Message::Close(None))]));
    drain.stop("the fabricated socket").await;
}

#[tokio::test]
async fn a_background_drain_says_nothing_about_a_stream_that_is_still_running() {
    let drain = BackgroundDrain::spawn(futures_util::stream::pending::<
        Result<Message, tokio_tungstenite::tungstenite::Error>,
    >());
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    drain.stop("the fabricated socket").await;
}

#[tokio::test]
#[should_panic(expected = "the venue CLOSED the idle socket while the test waited")]
async fn a_socket_drained_alongside_other_work_reports_its_own_close() {
    let mut idle = futures_util::stream::iter(vec![Ok(Message::Close(None))]);
    // The work never completes, so the only way out is the idle socket's close -
    // which must be named as a close rather than leaving the caller to blame
    // whatever it was about to assert.
    while_draining(&mut idle, std::future::pending::<()>(), "the idle socket").await;
}

#[tokio::test]
async fn work_finishing_first_is_what_while_draining_returns() {
    let mut idle =
        futures_util::stream::pending::<Result<Message, tokio_tungstenite::tungstenite::Error>>();
    let answer = while_draining(&mut idle, std::future::ready(7_u8), "the idle socket").await;
    assert_eq!(
        answer, 7,
        "the drained socket does not eat the work's value"
    );
}

/// Drains `socket` until the venue accepts `client_order_id`, which is the
/// condition "the venue has booked this order" stated on the wire. Every use
/// replaced a fixed sleep that was standing in for it.
async fn await_acceptance(socket: &mut WsSocket, client_order_id: &str) {
    let deadline = common::deadline(common::TEST_WALL_BUDGET);
    loop {
        let message = tokio::time::timeout_at(deadline, socket.next())
            .await
            .unwrap_or_else(|_| panic!("the venue accepted {client_order_id} within 30 s"))
            .unwrap_or_else(|| {
                panic!("the venue CLOSED this socket before accepting {client_order_id}")
            })
            .expect("a well-formed frame");
        let Message::Text(text) = message else {
            continue;
        };
        match serde_json::from_str::<VenueMessage>(&text) {
            Ok(VenueMessage::OrderAccepted {
                client_order_id: accepted,
                ..
            }) if accepted == client_order_id => return,
            Ok(VenueMessage::OrderRejected {
                client_order_id: rejected,
                reason,
                ..
            }) if rejected == client_order_id => panic!("{client_order_id} was rejected: {reason}"),
            _ => {}
        }
    }
}

/// Every trade in `[start, end]`, paged - never a prefix of them.
///
/// `/trades` fills its page from the oldest end of the window and stops at the
/// limit, so one query over a window wider than a page returns the window's
/// oldest prints and says nothing about it. A statistic taken over that - a
/// minimum, a last price - is then a statistic over stale water the market has
/// since moved through, and it is silently wrong rather than absent. Round 2 of
/// the test hunt found a live instance asserting favourable slippage that never
/// happened.
///
/// The cursor obeys the frontier rule: a full page's last instant may be cut off
/// mid-instant, so that whole instant is dropped and the next query resumes at
/// it rather than past it. A timestamp-only cursor may only advance onto an
/// instant once every row at that instant has been seen.
fn trade_window(base: &str, symbol: &str, start: u64, end: u64) -> Vec<TradeTick> {
    trade_window_paged(base, symbol, start, end, mogwai_protocol::MAX_HISTORY_LIMIT)
}

/// [`trade_window`] with the page size named, so the paging itself can be
/// exercised on a window a real page would swallow whole.
fn trade_window_paged(
    base: &str,
    symbol: &str,
    start: u64,
    end: u64,
    page_size: usize,
) -> Vec<TradeTick> {
    let page_size = page_size.max(1);
    let mut out: Vec<TradeTick> = Vec::new();
    let mut cursor = start;
    loop {
        let (status, body) = http_get(
            base,
            &format!("/operator/trades?symbol={symbol}&start={cursor}&end={end}&limit={page_size}"),
        );
        assert_eq!(status, 200, "the tape window answers: {body}");
        let page: Vec<TradeTick> = serde_json::from_str(&body).expect("a page of tape");
        if page.len() < page_size {
            out.extend(page);
            return out;
        }
        let boundary = page.last().expect("a full page is non-empty").ts_event;
        let kept: Vec<TradeTick> = page
            .into_iter()
            .filter(|trade| trade.ts_event < boundary)
            .collect();
        // A full page carrying one instant cannot be paged by a timestamp
        // cursor at all, and silently advancing past it would drop rows. It
        // cannot happen on a generated tape - said out loud rather than
        // absorbed, because absorbing it is the defect this helper exists for.
        assert!(
            !kept.is_empty(),
            "a whole {page_size}-row page sits at the single instant {boundary}, so this window \
             cannot be paged by timestamp"
        );
        cursor = boundary;
        out.extend(kept);
    }
}

/// The helper the slippage floor rests on is itself checked, against the venue.
///
/// `trade_window` exists because a single `/trades` query silently returns the
/// window's oldest prints once the page fills, and a statistic over that is
/// wrong rather than absent. That is a claim about paging, and a helper whose
/// paging is wrong fails the same way its caller did - quietly, with a plausible
/// number. So it is pinned against the answer the venue gives when the page
/// cannot truncate at all: a window small enough for one full page, fetched at
/// page size 3, must equal the same window fetched in one query.
///
/// The three-row page is what makes this bite. It forces thousands of cursor
/// advances over a window a real page swallows whole, so a cursor that drops or
/// duplicates a row at a page boundary shows up here where `MAX_HISTORY_LIMIT`
/// would never exercise the loop at all - measured, dropping the boundary row
/// turned 13780 prints into 9187.
///
/// What this does not cover, stated because a green test is otherwise read as
/// covering everything: the colliding half of the cursor rule. `trade_window`
/// resumes at a full page's last instant rather than past it, because rows at
/// that instant may have been cut off - but this tape stamps every print at a
/// distinct nanosecond, asserted below, so the two forms are indistinguishable
/// on it. Advancing past the boundary was tried as a bite-check and passed. The
/// defensive form is kept because a merged river or a coarser tape would collide
/// and nothing here would say so.
#[test]
#[ignore = "binds a loopback listener"]
fn a_paged_tape_window_equals_the_same_window_read_in_one_query() {
    let venue = spawn(&["--config", &fast_config()]);
    // The venue clock, not a boat's: nothing has connected, so no boat is
    // placed, and this is only a window bound rather than a claim about who
    // published what.
    let (_, clock_body) = http_get(&venue.http_base(), "/clock");
    let clock: mogwai_protocol::VenueClock =
        serde_json::from_str(&clock_body).expect("the venue clock");
    let end = clock.venue_now_ns;
    let start = venue.record.data_origin_ns;

    let (status, body) = http_get(
        &venue.http_base(),
        &format!(
            "/operator/trades?symbol={}&start={start}&end={end}&limit={}",
            venue.symbol,
            mogwai_protocol::MAX_HISTORY_LIMIT
        ),
    );
    assert_eq!(status, 200, "the whole window answers: {body}");
    let whole: Vec<TradeTick> = serde_json::from_str(&body).expect("the whole window");
    // The premise: this window fits one page, so the single query is the truth
    // to compare against. If a wider tape breaks that, the comparison would be
    // against a truncated answer and would pass by agreeing with a wrong one.
    assert!(
        whole.len() < mogwai_protocol::MAX_HISTORY_LIMIT,
        "this window no longer fits one page, so the single query is not a reference answer"
    );
    assert!(
        whole.len() > 3,
        "the window carries {} prints, too few to page at all",
        whole.len()
    );

    // The premise of the paragraph above: distinct instants. If this ever
    // fails, the colliding half of the cursor rule became reachable and owes a
    // gate of its own rather than the docstring's disclaimer.
    assert!(
        whole
            .windows(2)
            .all(|pair| pair[0].ts_event < pair[1].ts_event),
        "two prints share an instant, so the page boundary can now cut an instant in half"
    );

    let paged = trade_window_paged(&venue.http_base(), &venue.symbol, start, end, 3);
    assert_eq!(
        paged.len(),
        whole.len(),
        "paging the window at three rows a page neither dropped nor duplicated prints"
    );
    for (index, (paged, whole)) in paged.iter().zip(whole.iter()).enumerate() {
        assert_eq!(
            (paged.ts_event, paged.price),
            (whole.ts_event, whole.price),
            "print {index} differs between the paged read and the single query"
        );
    }
}

/// The last market instant this socket has actually been handed, draining
/// whatever is already queued on it.
///
/// A socket's own delivery is its own knowledge, which is why this exists rather
/// than a clock read. `/clock` is a run fact and the run clock runs ahead of what
/// any boat has published, so it is not a lower bound on what a handler could
/// have read; the frames this socket already holds are. Returns the tape origin
/// when nothing has arrived yet, which is a true lower bound too.
async fn drain_last_market_ts(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> u64 {
    let mut last = 0;
    let until = tokio::time::Instant::now() + Duration::from_millis(50);
    while let Ok(Some(Ok(message))) = tokio::time::timeout_at(until, socket.next()).await {
        if let Message::Text(text) = message {
            match serde_json::from_str::<VenueMessage>(&text) {
                Ok(VenueMessage::Trade(trade)) => last = last.max(trade.ts_event),
                Ok(VenueMessage::Quote(quote)) => last = last.max(quote.ts_event),
                _ => {}
            }
        }
    }
    last
}

/// The run's own `venue_now_ns`, so a test can wait on the venue's clock instead
/// of on the host's. `/clock` is a run fact and names no river, so callers pass
/// the bare path.
fn venue_sim_now(base: &str, clock_path: &str) -> u64 {
    let (status, body) = http_get(base, clock_path);
    assert_eq!(status, 200, "the venue clock answers: {body}");
    let clock: mogwai_protocol::VenueClock =
        serde_json::from_str(&body).expect("the clock answer parses");
    clock.venue_now_ns
}

/// One blocking `POST /control/divergence`, returning the status code.
fn post_divergence(base: &str, body: &str) -> u16 {
    use std::io::{Read, Write};
    let authority = base.trim_start_matches("http://");
    let mut stream = std::net::TcpStream::connect(authority).expect("connect");
    let request = format!(
        "POST /control/divergence HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).expect("send");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read");
    String::from_utf8_lossy(&raw)
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .expect("a status line")
}

/// Commands act in socket arrival order even when their modeled act latencies
/// differ. A cancel that arrives behind its submit must not overtake it.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn websocket_commands_cannot_overtake_each_other() {
    let venue = spawn(&["--config", &fast_config()]);
    assert_eq!(
        post_divergence(
            &venue.http_base(),
            r#"{"type":"CommandLatency","submit_act_ms":200,"cancel_act_ms":0}"#,
        ),
        202
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("open order socket");
    let submit = format!(
        r#"{{"type":"SubmitOrder","client_order_id":"ORDERED-1","symbol":"{}","side":"Buy","order_type":"Limit","quantity":"0.01","price":"1","time_in_force":"Gtc"}}"#,
        venue.symbol
    );
    socket
        .send(Message::Text(submit.into()))
        .await
        .expect("submit");
    socket
        .send(Message::Text(
            r#"{"type":"CancelOrder","client_order_id":"ORDERED-1"}"#.into(),
        ))
        .await
        .expect("cancel");

    let deadline = common::deadline(Duration::from_secs(10));
    let mut accepted = false;
    loop {
        let message = tokio::time::timeout_at(deadline, socket.next())
            .await
            .expect("ordered command result before deadline")
            .expect("socket remains open")
            .expect("valid websocket frame");
        let Message::Text(text) = message else {
            continue;
        };
        match serde_json::from_str::<VenueMessage>(&text) {
            Ok(VenueMessage::OrderAccepted {
                client_order_id, ..
            }) if client_order_id == "ORDERED-1" => accepted = true,
            Ok(VenueMessage::OrderCanceled {
                client_order_id, ..
            }) if client_order_id == "ORDERED-1" => {
                assert!(accepted, "cancel completed before its submit");
                break;
            }
            Ok(VenueMessage::OrderCancelRejected {
                client_order_id,
                reason,
                ..
            }) if client_order_id == "ORDERED-1" => {
                panic!("cancel overtook submit and was rejected: {reason}")
            }
            _ => {}
        }
    }
}

/// The per-connection command queue is bounded, and overflowing it is refused
/// rather than buffered - with no modeled act latency anywhere, so nothing but
/// the venue's own dispatch rate is holding the queue.
///
/// What this used to be, and why it was not a gate: 50 submits fired in a burst,
/// then a read for the refusal. Whether a one-deep queue ever overflowed was a
/// race between the consumer's send rate and the dispatcher's drain rate, with no
/// condition controlling it - and it got more reliable under load, which is the
/// worst kind of reliability, because the arm that would catch a regression is
/// the one that only fires on an idle host. It also fired all 50 sends before
/// reading a byte, so the venue's writer could be backpressured by this very
/// test while it waited to be told about capacity.
///
/// What replaces the bet is sustained pressure with a stated stopping rule. One
/// task sends continuously while another drains, and the loop ends on the first
/// refusal, on the deadline, or at a blast-radius cap - and the failure says
/// which, together with how much was sent and answered, so "the venue kept up"
/// and "the venue never refuses" are distinguishable. The cap is not a bet on
/// 50: a dispatcher that awaits a market reading, an engine lock and a match per
/// command cannot drain faster than a local socket write can fill for thousands
/// of commands running, so a refusal that never comes is the property failing.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn websocket_command_work_is_bounded_without_an_act_delay() {
    /// Bounds the damage if the property is broken - without it a broken bound
    /// spends the whole deadline writing commands at line rate.
    const MAX_PRESSURE: u64 = 5_000;
    let config = format!(
        "{}/tests/configs/command-cap.toml",
        env!("CARGO_MANIFEST_DIR")
    );
    let venue = spawn(&["--config", &config]);
    let (socket, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("open order socket");
    let (mut sink, mut stream) = socket.split();

    let deadline = common::deadline(Duration::from_secs(10));
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sender_stop = std::sync::Arc::clone(&stop);
    let symbol = venue.symbol.clone();
    let sender = tokio::spawn(async move {
        let mut sent = 0_u64;
        while sent < MAX_PRESSURE
            && !sender_stop.load(std::sync::atomic::Ordering::Relaxed)
            && tokio::time::Instant::now() < deadline
        {
            let command = format!(
                r#"{{"type":"SubmitOrder","client_order_id":"CAP-{sent}","symbol":"{symbol}","side":"Buy","order_type":"Market","quantity":"0.01","time_in_force":"Gtc"}}"#
            );
            // A closed socket is not a send failure to swallow: the venue
            // dropping this connection is a different outcome from refusing a
            // command, and the reader below reports it.
            if sink.send(Message::Text(command.into())).await.is_err() {
                break;
            }
            sent += 1;
        }
        sent
    });

    let mut answers = 0_u64;
    let outcome = loop {
        match tokio::time::timeout_at(deadline, stream.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                answers += 1;
                match serde_json::from_str::<VenueMessage>(&text) {
                    Ok(VenueMessage::AdmissionRejected { reason, .. })
                        if reason == "venue command capacity exhausted" =>
                    {
                        break Ok(());
                    }
                    Ok(VenueMessage::AdmissionRejected { reason, .. }) => {
                        break Err(format!("a different bound refused first: {reason}"));
                    }
                    _ => {}
                }
            }
            Ok(Some(Ok(_))) => {}
            Ok(Some(Err(err))) => break Err(format!("the socket failed in transport: {err}")),
            Ok(None) => break Err("the venue closed the socket instead of refusing".to_owned()),
            Err(_) => break Err("the deadline expired".to_owned()),
        }
    };
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let sent = sender.await.expect("the sending task");
    if let Err(why) = outcome {
        panic!(
            "no capacity refusal on a one-deep command queue: {why} after {sent} commands sent \
             and {answers} text frames drained (the live tape is most of that count). A venue \
             that never refused either drained commands faster than this could send them - which \
             a market submit, with its reading, its engine lock and its match, cannot - or is not \
             bounding per-connection command work at all"
        );
    }
}

#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn websocket_rejects_messages_over_the_protocol_ceiling() {
    let venue = spawn(&["--config", &fast_config()]);
    let (mut socket, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("open socket");
    socket
        .send(Message::Text(
            "x".repeat(mogwai_protocol::MAX_INBOUND_MESSAGE_BYTES + 1)
                .into(),
        ))
        .await
        .expect("send oversized message");
    // The socket is a live market-data feed, so frames the venue had already
    // written when the oversized frame landed are still in flight and arrive
    // first: the assertion is that the connection ends, not that the close is
    // the very next frame. It must still end promptly, so the deadline is the
    // real assertion - a venue that kept serving this connection indefinitely
    // would time out here rather than quietly pass.
    let deadline = common::deadline(Duration::from_secs(5));
    let mut ended = false;
    while !ended {
        let message = tokio::time::timeout_at(deadline, socket.next())
            .await
            .expect("the oversized frame ends the connection before the deadline");
        match message {
            Some(Ok(Message::Close(_)) | Err(_)) | None => ended = true,
            Some(Ok(Message::Text(text))) => {
                // Market data may precede the close. What must never appear is
                // a protocol-level answer to the oversized input: that would
                // mean the venue reassembled and parsed it.
                assert!(
                    !text.contains("ProtocolError"),
                    "the venue parsed a message over its own ceiling: {text}"
                );
            }
            Some(Ok(_)) => {}
        }
    }
}

/// One `POST /control/divergence`, returning the status code and the body,
/// because a refusal that must name something is only half asserted by its
/// status.
fn post_divergence_body(base: &str, body: &str) -> (u16, String) {
    use std::io::{Read, Write};
    let authority = base.trim_start_matches("http://");
    let mut stream = std::net::TcpStream::connect(authority).expect("connect");
    let request = format!(
        "POST /control/divergence HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).expect("send");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read");
    let text = String::from_utf8_lossy(&raw).to_string();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .expect("a status line");
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_owned())
        .unwrap_or_default();
    (status, body)
}

/// Generator havoc no longer needs an empty boatyard, and that is the whole
/// point of the fork.
///
/// This request used to be a `400` on the control plane whenever the named
/// river carried a boat, because arming meant mutating water a passenger might
/// already be reading. On a shared exchange some river nearly always has a
/// boat, so generator havoc was refused in exactly the mode it was most needed.
/// The arm now selects a river rather than changing one, so a passenger can
/// board surged water while another passenger is already reading the clean
/// river of the same label - concurrently, with neither seeing the other's
/// weather.
///
/// A socket places the first boat here rather than the boot river, because no
/// river is boated at boot: a version of this that never boarded would exercise
/// the empty-boatyard path and prove nothing about coexistence.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn a_generator_arm_boards_beside_a_boat_already_reading_that_label() {
    let venue = spawn(&["--config", &fast_config()]);
    let (_clean, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("board the clean river so the label carries a boat");
    let (_surged, _) = tokio_tungstenite::connect_async(format!(
        "{}&surge_start_ms=0&surge_duration_ms=60000&surge_rate_mult=4&surge_children_mult=2",
        venue.ws_url_for(&venue.symbol)
    ))
    .await
    .expect("an armed passenger boards its own river beside a boat on the same label");
}

/// A neutral arm is not an arm, so it boards the clean river rather than a
/// river of its own.
///
/// The cap does not evict, so a spelling that changes no generated byte must
/// not strand a river. Observable only as an acceptance here - which river a
/// passenger got is deliberately not visible on the wire, and the identity half
/// is pinned by `a_generator_arm_forks_the_river_under_one_label` in
/// `mogwai-venue`.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn a_neutral_generator_arm_upgrades_like_no_arm_at_all() {
    let venue = spawn(&["--config", &fast_config()]);
    let (_socket, _) = tokio_tungstenite::connect_async(format!(
        "{}&surge_start_ms=0&surge_duration_ms=60000&surge_rate_mult=1&surge_children_mult=1",
        venue.ws_url_for(&venue.symbol)
    ))
    .await
    .expect("a neutral arm is a legal request that boards the clean river");
}

/// A passenger reads its own history over its own socket, and pages through it.
///
/// This is the whole point of moving history off the symbol-keyed HTTP routes:
/// the request carries no symbol, so it cannot name the wrong river once one
/// label names several. What it proves end to end is the paging contract - the
/// cutoff is fixed at the first page and carried, the continuation resumes
/// strictly after the last row rather than at it, and a completed session says
/// so rather than trailing off.
///
/// A socket is attached to the live tape at upgrade, so live market frames are
/// already arriving while this runs. The loop therefore drains to a deadline
/// looking for the correlated reply rather than asserting on the next frame,
/// which would be asserting on whatever the tape happened to publish.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn a_passenger_pages_its_own_history_over_its_own_socket() {
    let venue = spawn(&["--config", &fast_config()]);
    let (mut socket, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("board");

    let mut seen = 0usize;
    let mut cutoff: Option<u64> = None;
    let mut last_ts: Option<u64> = None;
    let mut continuation: Option<String> = None;
    let mut pages = 0usize;
    loop {
        let request = continuation.as_ref().map_or_else(
            || r#"{"type":"QueryHistory","request_id":"H-1","kind":"Trades"}"#.to_owned(),
            |token| {
                format!(
                    r#"{{"type":"QueryHistory","request_id":"H-1","kind":"Trades","continuation":"{token}"}}"#
                )
            },
        );
        socket
            .send(Message::Text(request.into()))
            .await
            .expect("ask for a page of this passenger's own history");

        let deadline = common::deadline(common::TEST_WALL_BUDGET);
        let page = loop {
            let message = tokio::time::timeout_at(deadline, socket.next())
                .await
                .expect("the history request is answered")
                .expect("the socket stays open")
                .expect("a well-formed frame");
            let Message::Text(text) = message else {
                continue;
            };
            match serde_json::from_str::<VenueMessage>(&text) {
                Ok(VenueMessage::HistoryPage {
                    request_id,
                    rows,
                    cutoff,
                    continuation,
                    complete,
                    ..
                }) => {
                    assert_eq!(request_id, "H-1", "the reply is correlated to the request");
                    break (rows, cutoff, continuation, complete);
                }
                Ok(VenueMessage::HistoryRejected { reason, .. }) => {
                    panic!("the venue refused a history page: {reason}")
                }
                _ => continue,
            }
        };
        let (rows, page_cutoff, next, complete) = page;
        pages += 1;

        // One cutoff for the whole session. Recomputed per page it would move
        // with the run present, and a consumer paginating a live river would
        // never reach the end because each page pushed the finish line out.
        match cutoff {
            None => cutoff = Some(page_cutoff),
            Some(fixed) => assert_eq!(
                page_cutoff, fixed,
                "the session's cutoff moved between pages"
            ),
        }

        for row in &rows {
            let ts = row.ts_event();
            assert!(
                ts <= page_cutoff,
                "a row at {ts} is past the session's cutoff {page_cutoff}"
            );
            if let Some(previous) = last_ts {
                // Strictly increasing across the page boundary too, which is
                // what the continuation resuming after - rather than at - the
                // last row buys. Resuming at it would re-deliver one row per
                // page, and that duplicate is invisible to a consumer folding
                // bars.
                assert!(
                    ts > previous,
                    "history went backwards or repeated a row at {ts} after {previous}"
                );
            }
            last_ts = Some(ts);
        }
        seen += rows.len();

        if complete {
            assert!(
                next.is_none(),
                "a completed session hands back no continuation"
            );
            break;
        }
        continuation = Some(next.expect("an incomplete page carries a continuation"));
        assert!(pages < 64, "the session never completed");
    }

    assert!(
        seen > 0,
        "the warmup span must hold trades, or this test asserts nothing about paging"
    );
}

/// A continuation the venue did not issue is refused, correlated, rather than
/// answered from a position read out of it.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn a_fabricated_history_continuation_is_refused() {
    let venue = spawn(&["--config", &fast_config()]);
    let (mut socket, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("board");
    socket
        .send(Message::Text(
            r#"{"type":"QueryHistory","request_id":"H-2","kind":"Trades","continuation":"nonsense"}"#
                .into(),
        ))
        .await
        .expect("send a token the venue never minted");

    let deadline = common::deadline(common::TEST_WALL_BUDGET);
    loop {
        let message = tokio::time::timeout_at(deadline, socket.next())
            .await
            .expect("the request is answered")
            .expect("the socket stays open")
            .expect("a well-formed frame");
        let Message::Text(text) = message else {
            continue;
        };
        match serde_json::from_str::<VenueMessage>(&text) {
            Ok(VenueMessage::HistoryRejected {
                request_id,
                retryable,
                ..
            }) => {
                assert_eq!(request_id, "H-2", "the refusal names the request");
                assert!(
                    !retryable,
                    "an unreadable token cannot succeed by being asked again"
                );
                return;
            }
            // The failure this shape exists to prevent: an unreadable token
            // must not become a page, and least of all an empty one, which a
            // consumer cannot tell from a quiet market.
            Ok(VenueMessage::HistoryPage { .. }) => {
                panic!("a fabricated continuation was answered with a page")
            }
            _ => continue,
        }
    }
}

/// A malformed arm is refused before the account is claimed.
///
/// The ordering matters and is not incidental: an upgrade that evicted the
/// incumbent on its way to a `400` would let a consumer knock its own peer off
/// an account by sending a bad multiplier. The refusal names the bound it
/// broke, so a caller can tell this from the several other reasons an upgrade
/// is refused.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn a_generator_arm_outside_its_bounds_is_refused_on_the_upgrade() {
    let venue = spawn(&["--config", &fast_config()]);
    let refused = tokio_tungstenite::connect_async(format!(
        "{}&surge_start_ms=0&surge_duration_ms=60000&surge_rate_mult=4000&surge_children_mult=2",
        venue.ws_url_for(&venue.symbol)
    ))
    .await;
    let Err(error) = refused else {
        panic!("a rate_mult past the ceiling must not upgrade");
    };
    let tokio_tungstenite::tungstenite::Error::Http(response) = error else {
        panic!("the refusal is an HTTP status before the upgrade, got {error}");
    };
    assert_eq!(response.status(), 400, "the arm is refused: {response:?}");
    let body = String::from_utf8_lossy(response.body().as_deref().unwrap_or_default()).into_owned();
    assert!(
        body.contains("rate_mult"),
        "the refusal is the generator bound rather than some other 400: {body}"
    );
}

/// An evicted passenger ends its ride without the peer's cooperation.
///
/// `evict_account` writes a close frame and retires the lane, but the evicted
/// socket's own read loop used to leave only on the peer's close, the peer's
/// EOF, or the run ending. A consumer that ignores its close frame - or is merely
/// slow to act on it - therefore kept its `Passenger`, and with it the
/// account's ride on that boat's cadence, for as long as it liked. The next
/// connection wanting that account at a different speed was then refused with
/// "already seated", by a passenger the venue had already thrown off.
///
/// The evicted socket is never read after the eviction, deliberately: reading
/// it is what a cooperative consumer does, and a test that reads it is testing
/// the cooperative path. It is held in scope so its TCP connection stays up.
///
/// The final connect is polled because the ride ends in the evicted
/// passenger's teardown and there is no venue surface that reports it. The
/// distinction the poll rests on is not a margin: under the defect the ride is
/// held until the peer acts and no amount of waiting frees it, so a bounded
/// retry separates "released" from "held forever" rather than betting on how
/// fast a host is.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn an_evicted_passenger_gives_up_its_cadence_without_the_peer_reading() {
    let venue = spawn(&["--config", &fast_config()]);
    let at = |callsign: &str, speed: &str| {
        format!(
            "{}?account=WYRD-920&callsign={callsign}&speed={speed}",
            venue.ws_url()
        )
    };
    let served = async |socket: &mut WsSocket| {
        let deadline = common::deadline(Duration::from_secs(10));
        loop {
            let message = tokio::time::timeout_at(deadline, socket.next())
                .await
                .expect("the venue serves this socket a frame")
                .expect("the socket stays open")
                .expect("a well-formed frame");
            if matches!(message, Message::Text(_)) {
                return;
            }
        }
    };

    let (mut ignored, _) = tokio_tungstenite::connect_async(at("alpha", "1"))
        .await
        .expect("the first passenger boards at speed 1");
    served(&mut ignored).await;

    let (mut evicting, _) = tokio_tungstenite::connect_async(at("beta", "1"))
        .await
        .expect("a second consumer claims the account at the same speed");
    served(&mut evicting).await;

    // The evicting socket leaves properly, so the only thing that can still be
    // holding the speed-1 ride is the passenger that was thrown off.
    evicting
        .close(None)
        .await
        .expect("close the evicting socket");
    while evicting.next().await.is_some() {}

    let deadline = common::wall_deadline(Duration::from_secs(10));
    let reconnected = loop {
        match tokio_tungstenite::connect_async(at("gamma", "2")).await {
            Ok((socket, _)) => break socket,
            Err(refusal) => assert!(
                std::time::Instant::now() < deadline,
                "the account never came free at a new cadence, so the socket the venue evicted is \
                 still holding its ride: {refusal}"
            ),
        }
    };
    drop(reconnected);
    drop(ignored);
}

/// A silent cancel naming an account searches that account's book and no
/// other.
///
/// Client order ids are consumer-chosen, so two subagents numbering their orders
/// from one collide on a shared exchange. The lookup walked every account and
/// took the first match, so a scenario cancelling `ORD-1` on one subagent could
/// cancel a stranger's `ORD-1` instead - and a silent cancel emits no lifecycle
/// event by design, so the victim would learn of it only by querying.
///
/// The subject is an account whose book is empty, and that is what makes the
/// statement deterministic rather than a coin flip. Two accounts both resting
/// the same id is the scenario, but it is not a testable one: which book an
/// unqualified walk finds first is `HashMap` iteration order, so a test built
/// that way passes against the defect half the time and a bite-check on it
/// proves nothing. Asking a book that holds nothing to cancel an id another
/// book does hold has one right answer whatever the order - a miss - and the
/// unqualified walk always reaches the holder, so the perturbation fires every
/// run.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn a_silent_cancel_naming_an_account_reaches_only_that_accounts_book() {
    let venue = spawn(&["--config", &fast_config()]);
    let (mut mine, _) =
        tokio_tungstenite::connect_async(format!("{}?account=WYRD-910", venue.ws_url()))
            .await
            .expect("open an account socket");
    mine.send(Message::Text(
        format!(
            r#"{{"type":"SubmitOrder","client_order_id":"COLLIDE-1","symbol":"{}","side":"Buy","order_type":"Limit","quantity":"0.01","price":"1","time_in_force":"Gtc"}}"#,
            venue.symbol
        )
        .into(),
    ))
    .await
    .expect("submit");
    // Waited for before the control request, or the cancel below can
    // legitimately find nothing resting and the whole test passes vacuously.
    await_acceptance(&mut mine, "COLLIDE-1").await;

    let (status, body) = http_post_json(
        &venue.http_base(),
        "/accounts",
        r#"{"account_id":"WYRD-911","balances":{"USDT":"1000"}}"#,
    );
    assert_eq!(status, 201, "the stranger's account opens: {body}");

    let (status, body) = post_divergence_body(
        &venue.http_base(),
        r#"{"account":"WYRD-911","type":"CancelOpenOrderSilently","client_order_id":"COLLIDE-1"}"#,
    );
    assert_eq!(
        status, 404,
        "WYRD-911 rests no COLLIDE-1, so the request misses rather than reaching into WYRD-910's \
         book: {body}"
    );

    // And the order it must not have touched is still there, which the venue
    // states by accepting the properly-targeted cancel of it.
    let (status, body) = post_divergence_body(
        &venue.http_base(),
        r#"{"account":"WYRD-910","type":"CancelOpenOrderSilently","client_order_id":"COLLIDE-1"}"#,
    );
    assert_eq!(
        status, 202,
        "WYRD-910's own COLLIDE-1 was still resting, so the stranger's request did not take it: \
         {body}"
    );
}

/// And the miss path's diagnosis does not cancel either.
///
/// The scoped lookup above closes the search, and the round-3 cold review found
/// the same round's fix reintroducing the defect one line below it. When the
/// scoped search misses, the handler asks a ledger why, so it can tell "unknown
/// id" from "already terminal" - and it asked by running
/// `cancel_open_order_silently` and reading the `Err`. That call is not a query:
/// on `Ok` it closes the order out and reaps its held children. It also asked
/// the default account rather than the one the request named, which was safe
/// only while the search was unscoped, because then any id the default rested
/// had already been found and the diagnosis could only ever err.
///
/// So the holder here is the default account, which is exactly what the scoping
/// test above cannot express: its holder is a named account, so the default's
/// ledger has nothing to lose and the perturbation is invisible. A socket that
/// names no account rests the id; a named account that does not hold it is the
/// target; and the resting order must still be there afterwards.
///
/// The bite is the second cancel, not the 404. Both shapes answer `404 unknown
/// order` - under the defect because the cancel succeeded and `.err()` was
/// `None`. The only observable difference is whether the order survived, and an
/// unqualified cancel of it afterwards is how the venue states that: `202` if it
/// was still resting, `404` if the diagnosis ate it.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn a_missed_silent_cancel_diagnoses_without_cancelling_the_default_accounts_order() {
    let venue = spawn(&["--config", &fast_config()]);
    // No `account=`, so this socket trades the venue's default account - the
    // ledger the miss path used to diagnose off.
    let (mut mine, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("open a default-account socket");
    mine.send(Message::Text(
        format!(
            r#"{{"type":"SubmitOrder","client_order_id":"COLLIDE-2","symbol":"{}","side":"Buy","order_type":"Limit","quantity":"0.01","price":"1","time_in_force":"Gtc"}}"#,
            venue.symbol
        )
        .into(),
    ))
    .await
    .expect("submit");
    await_acceptance(&mut mine, "COLLIDE-2").await;

    let (status, body) = http_post_json(
        &venue.http_base(),
        "/accounts",
        r#"{"account_id":"WYRD-912","balances":{"USDT":"1000"}}"#,
    );
    assert_eq!(status, 201, "the stranger's account opens: {body}");

    let (status, body) = post_divergence_body(
        &venue.http_base(),
        r#"{"account":"WYRD-912","type":"CancelOpenOrderSilently","client_order_id":"COLLIDE-2"}"#,
    );
    assert_eq!(
        status, 404,
        "WYRD-912 rests no COLLIDE-2, so the request misses: {body}"
    );

    let (status, body) = post_divergence_body(
        &venue.http_base(),
        r#"{"type":"CancelOpenOrderSilently","client_order_id":"COLLIDE-2"}"#,
    );
    assert_eq!(
        status, 202,
        "the default account's COLLIDE-2 was still resting after the missed request, so the \
         diagnosis did not cancel it: {body}"
    );
}

/// A pulled snapshot does not open the account it reports on.
///
/// `GET /account?account=` is unauthenticated and resolved through the same
/// account id space `/ws` and `POST /accounts` use. It used to resolve through
/// the create-on-first-sight mint, so reading about an id created a ledger
/// under it - and the default `account_ttl_ms = 0` never collects one, so a
/// scanner walking ids left one ledger behind per id, permanently.
///
/// The observable is `POST /accounts`, not the snapshot, and that is the whole
/// design of this test. The snapshot's content is identical either way, by
/// construction - the preview is built from the same opening terms the mint
/// uses -
/// so asserting on the body would pass against both shapes and prove nothing.
/// `POST /accounts` refuses an id that is already open with a 409, so a 201
/// after the read is the venue stating that the read left nothing behind. The
/// 409 half is asserted too, on a second post, so the first assertion cannot be
/// read as `/accounts` simply never refusing.
#[test]
#[ignore = "binds a loopback listener"]
fn a_pulled_snapshot_does_not_open_the_account_it_reports_on() {
    let venue = spawn(&["--config", &fast_config()]);

    let (status, body) = http_get(&venue.http_base(), "/account?account=WYRD-READ");
    assert_eq!(status, 200, "an unopened account still answers: {body}");
    let snapshot: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        snapshot["account_id"], "WYRD-READ",
        "the answer is about the account that was asked for: {body}"
    );

    let (status, body) = http_post_json(
        &venue.http_base(),
        "/accounts",
        r#"{"account_id":"WYRD-READ","balances":{"USDT":"1000"}}"#,
    );
    assert_eq!(
        status, 201,
        "the read left no ledger behind, so the consumer's own open still states its terms: {body}"
    );

    let (status, body) = http_post_json(
        &venue.http_base(),
        "/accounts",
        r#"{"account_id":"WYRD-READ","balances":{"USDT":"1000"}}"#,
    );
    assert_eq!(
        status, 409,
        "and `/accounts` does refuse an id that IS open, so the 201 above is evidence: {body}"
    );

    // The consumer's own balance is what the account carries, not the venue's
    // configured opening balance a mint on the read path would have handed it.
    let (_, body) = http_get(&venue.http_base(), "/account?account=WYRD-READ");
    assert!(
        body.contains("1000"),
        "the opened ledger carries the consumer's balance: {body}"
    );
}

/// `CancelOpenOrderSilently` takes its clock from the targeted order: the id
/// already determines the order, hence its symbol and its river. A request that
/// also supplies a `symbol` may not disagree - the venue refuses rather than
/// silently preferring one of two answers - and an id naming no resting order
/// still gets the engine's own diagnosis.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn a_silent_cancel_naming_the_wrong_symbol_is_refused() {
    let venue = spawn(&["--config", &two_symbols_config()]);
    let (mut socket, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("open order socket");
    let submit = format!(
        r#"{{"type":"SubmitOrder","client_order_id":"SILENT-1","symbol":"{}","side":"Buy","order_type":"Limit","quantity":"0.01","price":"1","time_in_force":"Gtc"}}"#,
        venue.symbol
    );
    socket
        .send(Message::Text(submit.into()))
        .await
        .expect("submit");

    // A `/ws` socket is attached to the live tape on upgrade, so the accept is
    // drained for, never asserted on as the next frame.
    let deadline = common::deadline(Duration::from_secs(10));
    let mut resting = false;
    while !resting {
        let message = tokio::time::timeout_at(deadline, socket.next())
            .await
            .expect("the submit is acknowledged before the deadline")
            .expect("the socket stays open")
            .expect("a well-formed frame");
        if let Message::Text(text) = message {
            resting = text.contains("SILENT-1") && text.contains("OrderAccepted");
        }
    }

    let (status, body) = post_divergence_body(
        &venue.http_base(),
        r#"{"type":"CancelOpenOrderSilently","symbol":"MNQ","client_order_id":"SILENT-1"}"#,
    );
    assert_eq!(status, 400, "a mismatched symbol is refused: {body}");
    assert!(
        body.contains("MNQ"),
        "the refusal names the request: {body}"
    );
    assert!(
        body.contains(&*venue.symbol),
        "the refusal names the order's own river: {body}"
    );

    // The order is untouched by the refusal, and the matching request lands.
    let (status, body) = post_divergence_body(
        &venue.http_base(),
        &format!(
            r#"{{"type":"CancelOpenOrderSilently","symbol":"{}","client_order_id":"SILENT-1"}}"#,
            venue.symbol
        ),
    );
    assert_eq!(status, 202, "the matching symbol is accepted: {body}");

    // And an id naming nothing resting keeps the engine's own diagnosis rather
    // than a flattened "unknown order".
    let (status, body) = post_divergence_body(
        &venue.http_base(),
        r#"{"type":"CancelOpenOrderSilently","client_order_id":"SILENT-1"}"#,
    );
    assert_eq!(status, 404, "an already-cancelled order is refused: {body}");
    assert!(
        body.contains("terminal"),
        "the refusal distinguishes terminal from unknown: {body}"
    );
}

/// `/clock` is a run fact and says nothing about any boat.
///
/// The property under test is opacity, not the payload: a caller must not be
/// able to learn from this route that a boat exists, what cadence it runs, or
/// how far it has delivered. It used to answer on a named river's boat, which
/// made the route a boat-discovery surface - and with no speed named, another
/// account placing a faster boat moved every field of somebody else's answer.
///
/// So the two halves here are that the retired parameters are refused rather
/// than ignored, and that placing a second boat at a different cadence does not
/// move the answer at all.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn the_clock_is_a_run_fact_and_no_boat_moves_it() {
    let venue = spawn(&["--config", &two_symbols_config()]);

    for retired in [
        "/clock?symbol=MNQ",
        "/clock?speed=2",
        "/clock?symbol=MNQ&speed=2",
    ] {
        let (status, body) = http_get(&venue.http_base(), retired);
        assert_eq!(
            status, 400,
            "a retired clock parameter must be refused, not ignored: {retired} gave {body}"
        );
    }

    let (status, before) = http_get(&venue.http_base(), "/clock");
    assert_eq!(status, 200, "the clock answers: {before}");
    let before: mogwai_protocol::VenueClock = serde_json::from_str(&before).unwrap();

    // A second cadence on the same water is the case that used to be visible.
    // This socket boards at a speed nobody else is riding, so under the old
    // reducer it would have become the lead boat and moved the ceiling every
    // other caller was answered on.
    let (_socket, _) = tokio_tungstenite::connect_async(format!(
        "{}?symbol={}&speed=8",
        venue.ws_url(),
        venue.symbol
    ))
    .await
    .expect("a second cadence boards");

    let (status, after) = http_get(&venue.http_base(), "/clock");
    assert_eq!(status, 200, "the clock still answers: {after}");
    let after: mogwai_protocol::VenueClock = serde_json::from_str(&after).unwrap();
    assert_eq!(
        before.sim, after.sim,
        "the affine map is the run's and a boarding must not change it"
    );
    assert_eq!(
        before.data_origin_ns, after.data_origin_ns,
        "the tape floor is a run fact"
    );
    assert_eq!(
        before.warmup_ns, after.warmup_ns,
        "the warmup span is a run fact"
    );
}

/// A duration is a property of the passenger. One passenger's deadline closes
/// its own socket and leaves the boat carrying everyone else.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn a_passenger_duration_closes_one_socket_and_leaves_the_boat_running() {
    let venue = spawn(&["--config", &fast_config()]);
    let (staying, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("the indefinite passenger boards");
    let (mut leaving, _) = tokio_tungstenite::connect_async(format!(
        "{}?symbol={}&duration_ms=1500",
        venue.ws_url(),
        venue.symbol
    ))
    .await
    .expect("the bounded passenger boards the same boat");

    // `staying` is read continuously from here, not left parked while the
    // bounded passenger runs its 1.5 s down. This venue's fanout is a bounded
    // ring: a socket nobody reads loses market frames, so the old shape - drain
    // `leaving` for 1.5 s, then ask whether `staying` has a frame - could
    // observe that loss and report it as "one passenger's
    // deadline wound down the boat under another", sending the reader after a
    // serving defect that is not there. The reader task also reports how the
    // socket ended, so a close is named as a close.
    //
    // The socket is split so the test keeps the write half: the post-exit
    // evidence below is a round trip, which the reader alone cannot stage. What
    // crosses is a pair of monotone counts - query answers and tape prints - on
    // a `watch` that overwrites rather than a queue that grows. The queue shape
    // it replaced accumulated one entry per frame across a 1.5 s window of
    // unpaced firehose; monotone counts coalesce losslessly under a watch,
    // because the reader only ever compares them with `>`.
    let (mut staying_writer, mut staying_reads) = staying.split();
    let (staying_tx, mut staying_rx) = tokio::sync::watch::channel(Ok((0_u64, 0_u64)));
    let staying_reader = tokio::spawn(async move {
        let (mut answered, mut prints) = (0_u64, 0_u64);
        loop {
            let ended = match staying_reads.next().await {
                Some(Ok(Message::Text(text))) => {
                    match serde_json::from_str::<VenueMessage>(&text) {
                        Ok(VenueMessage::Trade(_) | VenueMessage::Quote(_)) => prints += 1,
                        Ok(VenueMessage::OrderStatusSnapshot(snapshot))
                            if snapshot.request_id == "AFTER-EXIT" =>
                        {
                            answered += 1;
                        }
                        _ => {}
                    }
                    if staying_tx.send(Ok((answered, prints))).is_err() {
                        return;
                    }
                    continue;
                }
                Some(Ok(Message::Close(frame))) => {
                    format!("the venue closed the indefinite passenger's socket: {frame:?}")
                }
                Some(Ok(_)) => continue,
                Some(Err(err)) => format!("the indefinite passenger's socket failed: {err}"),
                None => "the indefinite passenger's stream ended".to_string(),
            };
            drop(staying_tx.send(Err(ended)));
            return;
        }
    });

    // What it announces is the point, not merely that it announced. The run is
    // still going for `staying`, so a `RunComplete` here would tell this
    // consumer the venue had finished when only its own deadline had - which is
    // exactly what both completions sharing one frame used to say. The close
    // reason is checked too, so the frame and the close are held against each
    // other rather than each against the test's expectation.
    let deadline = common::deadline(common::TEST_WALL_BUDGET);
    let mut announced: Option<(u64, u64)> = None;
    let mut close_reason = None;
    loop {
        let message = tokio::time::timeout_at(deadline, leaving.next())
            .await
            .expect("the bounded passenger closes before the deadline");
        match message {
            Some(Ok(Message::Text(text))) => match serde_json::from_str::<VenueMessage>(&text) {
                Ok(VenueMessage::PassengerDurationComplete {
                    elapsed_ns,
                    declared_duration_ns,
                    ..
                }) => announced = Some((elapsed_ns, declared_duration_ns)),
                Ok(VenueMessage::RunComplete { .. }) => panic!(
                    "the bounded passenger announced a finished RUN; the run is still \
                         carrying the other passenger"
                ),
                _ => {}
            },
            Some(Ok(Message::Close(frame))) => {
                close_reason = frame.map(|frame| frame.reason.to_string());
                break;
            }
            Some(Err(_)) | None => break,
            Some(Ok(_)) => {}
        }
    }
    let (elapsed_ns, declared_duration_ns) =
        announced.expect("the bounded passenger announced its own duration before closing");
    assert_eq!(
        declared_duration_ns, 1_500_000_000,
        "the announcement states the deadline that fired"
    );
    assert!(
        elapsed_ns >= declared_duration_ns,
        "the observed span is measured, not the deadline restated: {elapsed_ns} < \
         {declared_duration_ns}"
    );
    assert_eq!(
        close_reason.as_deref(),
        Some(mogwai_protocol::close::DURATION_COMPLETE),
        "the close agrees with the frame rather than refining it"
    );

    // The boat is still carrying the other passenger. Establishing that takes a
    // print the venue produced after the exit, and the query is what makes one
    // provable: it goes out once the bounded passenger has closed, so its answer
    // cannot have been sitting in a buffer, and the stream's own ordering makes
    // everything the venue writes after that answer post-exit too. A print past
    // it is therefore a print the boat made with the other passenger gone.
    //
    // Neither cheaper shape establishes that. "Discard whatever is queued, then
    // take the next frame" empties only what the reader task has already
    // forwarded, so frames that reached the socket before the exit and had not
    // been polled yet land afterwards and are counted as post-exit evidence -
    // and this test is current-thread, so that scheduling lag is real rather
    // than theoretical. Comparing tape stamps against the venue's clock fails
    // for a different reason: `fast.toml` is `speed = 0.0`, where delivery is
    // unpaced while the boat clock is still built wall-rated, so `ts_event` runs
    // far ahead of `venue_now_ns` and says nothing about when a frame was
    // served.
    staying_writer
        .send(Message::Text(
            r#"{"type":"QueryOrders","request_id":"AFTER-EXIT","open_only":false}"#.into(),
        ))
        .await
        .expect("the indefinite passenger can still be heard");
    // Five seconds, not the thirty this file uses elsewhere, and deliberately:
    // both waits below are milliseconds of expected latency, and the whole test
    // has to finish inside the harness's per-test budget or its truthful message
    // is replaced by a hung-test kill - which is the same wrong-answer failure
    // it exists to avoid.
    let deadline = common::deadline(Duration::from_secs(5));
    let prints_at_answer = loop {
        let (answered, prints) = match *staying_rx.borrow_and_update() {
            Ok(counts) => counts,
            Err(ref why) => panic!("{why}"),
        };
        if answered > 0 {
            break prints;
        }
        tokio::time::timeout_at(deadline, staying_rx.changed())
            .await
            .expect("the venue answers the indefinite passenger within 5 s of the other's exit")
            .expect("the reader task ended without saying why");
    };
    let deadline = common::deadline(Duration::from_secs(5));
    loop {
        let (_, prints) = match *staying_rx.borrow_and_update() {
            Ok(counts) => counts,
            Err(ref why) => panic!("{why}"),
        };
        if prints > prints_at_answer {
            break;
        }
        tokio::time::timeout_at(deadline, staying_rx.changed())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "one passenger's deadline wound down the boat under another: 5 s after the \
                     bounded passenger left, the indefinite one has been served no print the \
                     venue can be shown to have made afterwards"
                )
            })
            .expect("the reader task ended without saying why");
    }
    staying_reader.abort();
}
