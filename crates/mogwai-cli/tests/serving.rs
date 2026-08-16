// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! L3-L6 gates: the history floor, the boot river's warmup span servable at
//! readiness, the
//! subscription-free feed, and the single ledger every connection shares.

mod common;

use std::time::Duration;

use common::{
    accelerated_config, band_config, fast_config, http_get, http_post_json, mnq_preset_config,
    paced_config, perpetual_config, spawn, tiny_fanout_config, two_symbols_config,
};
use futures_util::{SinkExt, StreamExt};
use mogwai_protocol::{LiquiditySide, ServerMessage, TradeTick};
use tokio_tungstenite::tungstenite::Message;

/// A config whose boot river is named ONLY by `[instrument] preset`, with no
/// top-level `symbol` key. The harness must resolve MNQ the way `serve.rs`
/// does; reading the raw config key and defaulting to `DEFAULT_PRESET` answers
/// BTCUSDT for a venue that serves MNQ, and no other config in this tree can
/// tell the two apart.
#[test]
#[ignore = "binds a loopback listener"]
fn preset_only_config_resolves_the_boot_river() {
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
fn history_is_served_for_a_configured_symbol_that_is_not_the_boot_river() {
    let venue = spawn(&["--config", &two_symbols_config()]);
    let (status, body) = http_get(&venue.http_base(), "/trades?symbol=MNQ&start=0&limit=5");
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

/// Piece 13: the upgrade refuses an ILLEGAL symbol, not an unconfigured one.
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
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while let Ok(Some(Ok(message))) = tokio::time::timeout_at(deadline, socket.next()).await {
        if matches!(message, Message::Text(_)) {
            return;
        }
    }
    panic!("the named boot river produced no frames");
}

#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn a_ws_upgrade_for_a_configured_non_boot_symbol_is_served() {
    let venue = spawn(&["--config", &two_symbols_config()]);
    let (mut socket, response) = tokio_tungstenite::connect_async(venue.ws_url_for("MNQ"))
        .await
        .expect("configured non-boot river places a boat");
    assert_eq!(response.status(), 101);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while let Ok(Some(Ok(Message::Text(frame)))) =
        tokio::time::timeout_at(deadline, socket.next()).await
    {
        if frame.contains("MNQ") {
            return;
        }
    }
    panic!("configured non-boot river produced no named market frame");
}

/// A boated river answers history only as far as ITS BOAT has published, never
/// as far as the venue clock has run. A boat placed `T` after boot sits
/// `T * speed` behind that clock permanently, so the venue clock is a
/// look-ahead oracle for every river but the boot one - which is why this test
/// must use a SECOND symbol: the boot river carries a boat from boot and lags
/// by nothing, so a boot-symbol test would pass under both ceilings.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn history_is_bounded_by_the_rivers_own_boat_not_the_venue_clock() {
    let venue = spawn(&["--config", &two_symbols_config()]);
    tokio::time::sleep(Duration::from_millis(250)).await;
    let (socket, _) =
        tokio_tungstenite::connect_async(format!("{}?symbol=MNQ&speed=1", venue.ws_url()))
            .await
            .expect("place the second river late");

    let (_, boat) = http_get(&venue.http_base(), "/clock?symbol=MNQ");
    let boat: mogwai_protocol::ServerClock = serde_json::from_str(&boat).unwrap();
    let (_, venue_clock) = http_get(&venue.http_base(), "/clock");
    let venue_clock: mogwai_protocol::ServerClock = serde_json::from_str(&venue_clock).unwrap();
    assert!(
        venue_clock.server_now_ns > boat.server_now_ns,
        "the test must construct a late boat"
    );

    // START-ANCHORED, deliberately: an unanchored request breaks at `limit`
    // from the history source's default position, so it can fill its page with
    // old rows and never reach either ceiling. Anchored one nanosecond above
    // the boat but far below the venue clock, the two ceilings give different
    // answers and only one of them can be right.
    let start = boat.server_now_ns.saturating_add(1);
    assert!(
        start < venue_clock.server_now_ns,
        "the anchor must sit between the two ceilings"
    );
    let (status, body) = http_get(
        &venue.http_base(),
        &format!("/trades?symbol=MNQ&start={start}&limit=5"),
    );
    assert_eq!(
        status, 400,
        "history admitted a start beyond the boat: {body}"
    );

    // And the ceiling did not collapse onto nothing: a start BELOW what the
    // boat published is still served. A test asserting only the refusal cannot
    // tell a correct bound from one that refuses everything.
    let (status, body) = http_get(
        &venue.http_base(),
        &format!(
            "/trades?symbol=MNQ&start={}&limit=5",
            venue.record.data_origin_ns
        ),
    );
    assert_eq!(
        status, 200,
        "history below the boat is still served: {body}"
    );
    drop(socket);
}

#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn an_order_for_another_symbol_is_refused_on_a_bound_socket() {
    let venue = spawn(&["--config", &fast_config()]);
    // A minute of modeled submit-act latency, which this venue's sim clock
    // realizes one-for-one in wall time. The mismatch is refused at the
    // protocol boundary, ABOVE that sleep and above the market reading,
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
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while let Ok(Some(Ok(message))) = tokio::time::timeout_at(deadline, socket.next()).await {
        let Message::Text(text) = message else {
            continue;
        };
        match serde_json::from_str::<ServerMessage>(&text) {
            Ok(ServerMessage::OrderRejected {
                client_order_id,
                reason,
                ..
            }) if client_order_id == "WRONG-RIVER" => {
                assert!(reason.contains("does not match the symbol this connection is bound to"));
                return;
            }
            Ok(ServerMessage::AdmissionRejected { .. }) => {
                panic!("a symbol mismatch was mislabeled as capacity")
            }
            Ok(ServerMessage::OrderAccepted {
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
    let preset = mogwai_server::config::profile_from_preset("BTCUSDT").unwrap();
    assert_eq!(def.class, preset.def.class);
    assert_eq!(def.price_precision, preset.def.price_precision);
    assert_eq!(def.size_precision, preset.def.size_precision);
    assert_eq!(def.price_increment, preset.def.price_increment);
    assert_eq!(def.size_increment, preset.def.size_increment);

    let (mut socket, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("connect websocket");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while let Ok(Some(Ok(message))) = tokio::time::timeout_at(deadline, socket.next()).await {
        if let Message::Text(text) = message
            && let Ok(ServerMessage::Trade(trade)) = serde_json::from_str(&text)
        {
            assert_eq!(trade.symbol.as_ref(), "FOOBAR");
            return;
        }
    }
    panic!("no FOOBAR trade arrived before the deadline");
}

#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn binary_client_frames_receive_a_protocol_error() {
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

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while let Ok(Some(Ok(message))) = tokio::time::timeout_at(deadline, socket.next()).await {
        if let Message::Text(text) = message
            && let Ok(ServerMessage::ProtocolError { reason, .. }) =
                serde_json::from_str::<ServerMessage>(&text)
        {
            assert!(reason.contains("binary client frames are unsupported"));
            return;
        }
    }
    panic!("no ProtocolError arrived before the liveness deadline");
}

#[tokio::test]
#[ignore = "binds a loopback listener and samples paced delivery"]
async fn tape_lateness_under_acceleration() {
    let venue = spawn(&["--config", &accelerated_config()]);
    let (status, body) = http_get(&venue.http_base(), "/clock");
    assert_eq!(status, 200);
    let clock: mogwai_protocol::ServerClock = serde_json::from_str(&body).unwrap();
    let (mut socket, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut lateness = Vec::new();
    while tokio::time::Instant::now() < deadline {
        let Some(Ok(Message::Text(text))) = tokio::time::timeout_at(deadline, socket.next())
            .await
            .ok()
            .flatten()
        else {
            break;
        };
        if let Ok(ServerMessage::Trade(trade)) = serde_json::from_str(&text) {
            let due = clock.sim.wall_ns(trade.ts_event);
            lateness.push(mogwai_protocol::now_unix_nanos().saturating_sub(due));
        }
    }
    assert!(!lateness.is_empty());
    lateness.sort_unstable();
    let p99 = lateness[(99 * lateness.len()).div_ceil(100) - 1];
    let max = *lateness.last().unwrap();
    eprintln!(
        "frames={} p99_lateness_ns={p99} max_lateness_ns={max}",
        lateness.len()
    );
    assert!(p99 <= 50_000_000, "p99 lateness {p99}ns exceeds 50ms");
}

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
        let clock: mogwai_protocol::ServerClock = serde_json::from_str(&body).unwrap();
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
            "/trades?symbol={}&start=0&end={}&limit=50",
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
        &format!("/trades?symbol={}&start={far_future}", venue.symbol),
    );
    assert_eq!(status, 400, "a future start is refused");
    assert!(
        body.contains("sim-now"),
        "the refusal names the clock: {body}"
    );

    // The asymmetry is deliberate and pinned here so it stays a decision: an
    // explicit END past the clock is the ordinary "everything up to now"
    // request written against the caller's own clock, so it is CLAMPED to
    // sim-now and served, not refused. See the comment in `http::trades`.
    let (status, body) = http_get(
        &venue.http_base(),
        &format!(
            "/trades?symbol={}&start={}&end={far_future}&limit=5",
            venue.symbol, venue.record.run_start_ns
        ),
    );
    assert_eq!(status, 200, "a future end is clamped, not refused: {body}");
}

/// Piece 13 REPLACED the unserved-symbol refusal on history: the only symbol a
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
            &format!("/{endpoint}?symbol=NOT%20A%20SYMBOL&start=0&limit=5"),
        );
        assert_eq!(status, 400, "an illegal symbol is refused: {body}");
        assert!(
            body.contains("illegal symbol"),
            "the refusal says what is wrong: {body}"
        );

        let (status, body) = http_get(
            &venue.http_base(),
            &format!("/{endpoint}?symbol=NOT-A-SYMBOL&start=0&limit=5"),
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
            &format!("/trades?symbol={lowercase}&start=0&limit=5"),
        );
        assert_eq!(status, 200, "a miscased label is its own river: {body}");
        assert!(
            body.contains(&lowercase),
            "and it is served under that label, not folded: {body}"
        );
    }

    let (status, body) = http_get(
        &venue.http_base(),
        &format!("/trades?symbol={}&start=0&limit=5", venue.symbol),
    );
    assert_eq!(
        status, 200,
        "the run's boot symbol remains servable: {body}"
    );
}

/// What proves the warmup was MATERIALIZED rather than merely declared: a
/// request for the earliest servable instant, issued the moment the readiness
/// line arrives, returns data instead of a refusal or an empty page.
#[test]
#[ignore = "binds a loopback listener"]
fn the_full_warmup_span_is_servable_at_readiness() {
    let venue = spawn(&["--config", &fast_config()]);
    let floor = venue.record.data_origin_ns;

    let (status, body) = http_get(
        &venue.http_base(),
        &format!("/trades?symbol={}&start={floor}&limit=50", venue.symbol),
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
/// sent by the client, and there is no subscribe frame left to send.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn a_connection_receives_the_tape_without_asking() {
    let venue = spawn(&["--config", &fast_config()]);
    let (mut socket, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("open a socket");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let message = tokio::time::timeout_at(deadline, socket.next())
            .await
            .expect("the venue pushes its tape unbidden")
            .expect("the socket stays open")
            .expect("a well-formed frame");
        if let Message::Text(text) = message
            && let Ok(ServerMessage::Trade(trade)) = serde_json::from_str(&text)
        {
            assert_eq!(trade.symbol.as_ref(), venue.symbol);
            return;
        }
    }
}

/// A venue declaring NO warmup still publishes its tape. The worker's opening
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

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let message = tokio::time::timeout_at(deadline, socket.next())
            .await
            .expect("a zero-warmup venue still pushes its tape")
            .expect("the socket stays open")
            .expect("a well-formed frame");
        if let Message::Text(text) = message
            && let Ok(ServerMessage::Trade(trade)) = serde_json::from_str(&text)
        {
            assert_eq!(trade.symbol.as_ref(), venue.symbol);
            assert_eq!(venue.record.warmup_ns, 0, "the fixture declares no warmup");
            return;
        }
    }
}

/// The lag policy STAYS through the tenancy rip - it bounds one connection's
/// memory, not one tenant's share - and it must be re-pinned against the new
/// top-level frame, since `SubscriptionIssue::FeedLagged` no longer exists to
/// carry it. A connection that falls behind the ring is told so and killed as a
/// venue fault, never served a silent hole.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn a_slow_connection_is_dropped_with_feed_lagged() {
    let venue = spawn(&["--config", &tiny_fanout_config()]);
    let (mut socket, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("open a socket");

    // Read SLOWLY rather than not at all. A peer that never reads wedges the
    // venue's writer, and the lag report rides that same outbound path - so a
    // fully stopped reader could not observe its own ejection. A slow reader is
    // both the realistic case and the one the policy is written for: it drains
    // just enough for the venue to tell it what it missed.
    // The slow reading is only needed to INDUCE the lag: a handful of 50 ms
    // stalls against an unpaced raw-fill tape overruns an 8-frame ring many
    // times over. After that the client drains flat out, because the report it
    // is waiting for sits BEHIND every frame the venue queued in the socket
    // before it noticed - and at ~8.5 raw fills per parent event that is far
    // more frames than a one-per-50-ms reader gets through inside the deadline.
    // Keeping the stall for the whole loop made this gate pass alone and fail
    // under load, which is a property of the reader, not of the venue.
    const STALLED_READS: usize = 10;
    let mut lagged = None;
    let mut close_code = None;
    let mut reads = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        if reads < STALLED_READS {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        reads += 1;
        let Ok(Some(Ok(message))) =
            tokio::time::timeout(Duration::from_secs(2), socket.next()).await
        else {
            break;
        };
        match message {
            Message::Text(text) => {
                if let Ok(ServerMessage::FeedLagged { skipped, .. }) =
                    serde_json::from_str::<ServerMessage>(&text)
                {
                    lagged = Some(skipped);
                }
            }
            Message::Close(frame) => {
                close_code = frame.map(|frame| u16::from(frame.code));
                break;
            }
            _ => {}
        }
    }

    let skipped = lagged.expect("the venue names the frames it lost rather than serving a hole");
    assert!(skipped > 0, "a lag report names a non-zero skip count");
    assert_eq!(
        close_code,
        Some(1011),
        "losing promised market data is a VENUE fault, not a client refusal"
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

    // The order is worked on one socket; the LEDGER it moved is the run's, so
    // the other socket's query answers from the same book.
    tokio::time::sleep(Duration::from_millis(500)).await;
    observer
        .send(Message::Text(
            r#"{"type":"QueryOrders","request_id":"Q-1","open_only":false}"#.into(),
        ))
        .await
        .expect("query the venue's truth");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let message = tokio::time::timeout_at(deadline, observer.next())
            .await
            .expect("the query is answered")
            .expect("the socket stays open")
            .expect("a well-formed frame");
        if let Message::Text(text) = message
            && let Ok(ServerMessage::OrderStatusSnapshot(snapshot)) =
                serde_json::from_str::<ServerMessage>(&text)
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

/// Two ACCOUNTS on one venue do not share a ledger, which is the converse of
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

    tokio::time::sleep(Duration::from_millis(500)).await;
    stranger
        .send(Message::Text(
            r#"{"type":"QueryOrders","request_id":"Q-2","open_only":false}"#.into(),
        ))
        .await
        .expect("query the second account's truth");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let message = tokio::time::timeout_at(deadline, stranger.next())
            .await
            .expect("the query is answered")
            .expect("the socket stays open")
            .expect("a well-formed frame");
        if let Message::Text(text) = message
            && let Ok(ServerMessage::OrderStatusSnapshot(snapshot)) =
                serde_json::from_str::<ServerMessage>(&text)
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

/// A client names its own opening balance, and that is the ledger it trades.
///
/// The venue's `[balances]` is what an UNNAMED account gets; it stops being the
/// balance of the one ledger. Two experiments sized differently are the case
/// this exists for, and they have to be runnable on one venue.
#[test]
#[ignore = "binds a loopback listener"]
fn an_account_opens_on_the_balance_its_client_named() {
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
        "the client's opening balance is the ledger's: {body}"
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
        "a client-named balance leaked into the default account: {body}"
    );
}

/// Re-opening a live account is refused rather than resetting it. An account
/// outlives its connections, so the request is ambiguous between a fresh
/// experiment and a reconnecting client re-sending its config - and the second
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
/// be JUDGED afterwards.
///
/// The audience is the EVALUATOR, not the strategy: mogwai presents no
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

/// A policy the venue cannot enforce is refused where it ENTERS, not hours
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

/// A policed SPOT account trades and is valued: the base asset it ends up
/// holding is priced by the pair that quotes it.
///
/// This is what the default tape shape needs. A spot fill credits the base as a
/// currency balance and debits the quote, so an account holding BTC is worth
/// nothing statable until BTCUSDT is marked - and its equity must NOT collapse
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

    // Let the fill book and at least one sweep pass mark the pair.
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let (status, body) = http_get(&venue.http_base(), "/account?account=WYRD-204");
    assert_eq!(status, 200, "the account answers: {body}");
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    let equity: f64 = value["risk"]["equity"]
        .as_str()
        .expect("equity is reported")
        .parse()
        .expect("equity parses");
    assert!(
        equity > 4_000_000.0,
        "buying an asset must not read as spending its notional: {body}"
    );
    assert!(
        value["risk"]["breached"].is_null(),
        "a purchase is not a drawdown breach: {body}"
    );
}

/// A second socket claiming a seated account evicts the first, and RESUMES its
/// ledger.
///
/// Both halves matter together. Eviction is what keeps an account on one river
/// with one reader: two sockets on one id would be one ledger written from two
/// places. Resuming is what makes a reconnect a continuation - the venue cannot
/// tell a returning client from a stranger presenting the id, so handing the
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

    // The incumbent is closed, and NORMALLY: an eviction is not a fault, and a
    // consumer that redialled on it would evict whatever evicted it.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut closed = false;
    while !closed {
        let message = tokio::time::timeout_at(deadline, first.next())
            .await
            .expect("the first socket is closed")
            .expect("a frame or a close")
            .expect("a well-formed frame");
        if let Message::Close(frame) = message {
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
            closed = true;
        }
    }

    // And the newcomer inherits the book rather than a fresh one.
    second
        .send(Message::Text(
            r#"{"type":"QueryOrders","request_id":"Q-3","open_only":false}"#.into(),
        ))
        .await
        .expect("query the resumed ledger");
    loop {
        let message = tokio::time::timeout_at(deadline, second.next())
            .await
            .expect("the query is answered")
            .expect("the socket stays open")
            .expect("a well-formed frame");
        if let Message::Text(text) = message
            && let Ok(ServerMessage::OrderStatusSnapshot(snapshot)) =
                serde_json::from_str::<ServerMessage>(&text)
        {
            assert_eq!(snapshot.request_id, "Q-3");
            assert!(
                snapshot
                    .orders
                    .iter()
                    .any(|order| order.client_order_id == "RESUMED-1"),
                "the returning connection did not get its own ledger back"
            );
            return;
        }
    }
}

/// A client asks for a policy BY NAME rather than restating it, and a name
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

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while let Ok(Some(Ok(message))) = tokio::time::timeout_at(deadline, socket.next()).await {
        let Message::Text(text) = message else {
            continue;
        };
        match serde_json::from_str::<ServerMessage>(&text) {
            Ok(ServerMessage::OrderRejected { reason, .. }) => {
                assert!(
                    reason.contains("may not carry more than 10"),
                    "the refusal names the cap: {reason}"
                );
                return;
            }
            Ok(ServerMessage::OrderAccepted { .. }) => {
                panic!("an oversized submit must not be accepted")
            }
            Ok(ServerMessage::AdmissionRejected {
                subject, reason, ..
            }) => panic!("the venue refused admission for {subject:?}: {reason}"),
            Ok(ServerMessage::ProtocolError { reason, .. }) => {
                panic!("the venue read the submit as malformed: {reason}")
            }
            _ => {}
        }
    }
    panic!("no position-cap OrderRejected arrived");
}

/// A blackout armed on one account does not blind another.
///
/// Transport havoc corrupts what one connection RECEIVES rather than what the
/// generator produces, so it rides the passenger. Run-wide was the old shape,
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
        // The ceiling, in SIMULATED ms. This config is accelerated, so a
        // wall-comfortable window has to be a large sim one.
        // The ceiling, in SIMULATED ms. This config is accelerated, so a
        // wall-comfortable window has to be a large sim one.
        r#"{"type":"GoDark","ms":3600000,"account":"WYRD-500"}"#,
    );
    assert_eq!(status, 202, "the targeted blackout arms: {body}");

    // BOTH sockets are drained before either is judged. A socket is attached to
    // the live tape on upgrade, so whatever the writer had queued when the
    // blackout armed is still in flight on both of them and says nothing about
    // whether a window is open. Asserting before this drain passes whichever way
    // the targeting is wired, which is the trap this comment exists to mark.
    // WHETHER A SOCKET GOES QUIET is the discriminator, and it has to be, because
    // neither "did it receive something" nor "was the next frame absent" can
    // tell the two apart: a socket is attached to the live tape on upgrade, so a
    // blacked-out one keeps delivering its backlog for a while and a live one
    // never goes quiet at all. Reading each to a gap gives a clean answer -
    // blacked out means the backlog exhausts and a gap appears, still served
    // means frames keep arriving until the deadline.
    async fn goes_quiet(
        socket: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> bool {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline {
            if tokio::time::timeout(Duration::from_millis(250), socket.next())
                .await
                .is_err()
            {
                return true;
            }
        }
        false
    }

    assert!(
        goes_quiet(&mut dark).await,
        "the targeted account kept receiving: its blackout never opened"
    );
    assert!(
        !goes_quiet(&mut lit).await,
        "an untargeted account went quiet: the blackout reached an account it did not name"
    );
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

    let (status, body) = http_get(
        &venue.http_base(),
        &format!("/clock?symbol={}&speed=2", venue.symbol),
    );
    assert_eq!(status, 200, "the slow boat's clock answers: {body}");
    let slow_clock: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(slow_clock["sim"]["speed"], 2.0);

    let (status, body) = http_get(
        &venue.http_base(),
        &format!("/clock?symbol={}&speed=3", venue.symbol),
    );
    assert_eq!(status, 200, "the fast boat's clock answers: {body}");
    let fast_clock: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(fast_clock["sim"]["speed"], 3.0);

    // Keep the sockets in scope so the boats stay seated while we read.
    drop(slow);
    drop(fast);
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
    let rendered = format!("{error}");
    assert!(
        rendered.contains("400") || rendered.contains("already seated"),
        "the refusal names the sitting cadence: {rendered}"
    );
}

/// A seat is given up when the SOCKET ends, not when the account freezes.
///
/// Two unnamed sockets share the default account, so closing one leaves the
/// account attached and never freezes it. If the departed socket's seat
/// survived, a reconnect on that river at a new cadence would be refused
/// against a cadence nobody is riding - and, because a boat key is just
/// (river, speed), the ledger would be handed the next boat placed there
/// whether or not it ever boarded.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn a_seat_is_released_when_its_socket_goes_even_though_the_account_stays() {
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

    // Poll: the close is asynchronous, and the seat is released as the
    // session unwinds rather than as the client's socket drops.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
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
                    "the vacated seat still refuses a new cadence: {error}"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}

/// An account not funded in what its symbol settles in is refused AT BIND,
/// naming the currency.
///
/// The venue's `[balances]` is only what an unnamed account opens with, so a
/// client that named its own funding cannot be checked at boot - the venue has
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

    // Through a real upgrade attempt: the refusal is a STATUS before the 101,
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
    let rendered = format!("{error}");
    assert!(
        rendered.contains("400") || rendered.contains("HTTP"),
        "the refusal is a status before the upgrade: {rendered}"
    );
}

/// A perpetual position PAYS FUNDING, which is the only thing tying a perp to
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
    tokio::time::sleep(Duration::from_millis(500)).await;

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
    tokio::time::sleep(Duration::from_millis(3_000)).await;
    let (_, after) = http_get(&venue.http_base(), "/account");
    let after = balance_of(&after);

    assert!(
        after < before,
        "a long perpetual must pay funding across an interval: {before} -> {after}"
    );
}

/// Two sockets that name NO account both live. Only a CLAIMED account evicts.
///
/// This is the shape the default account exists to serve, and the eviction
/// landing broke it: both sockets resolve to the default, so keying eviction on
/// the account alone made the second close the first - a client evicting itself
/// by opening a second socket. Naming an id is a statement about identity and
/// eviction is the answer to it; naming none is the client saying it has no
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
    for (label, socket) in [("first", &mut first), ("second", &mut second)] {
        let seen = tokio::time::timeout(Duration::from_secs(10), socket.next()).await;
        assert!(
            matches!(seen, Ok(Some(Ok(Message::Text(_))))),
            "the {label} socket stopped being served: {seen:?}"
        );
    }
}

/// An unpoliced account is enforced against nothing, which is what every client
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
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let message = tokio::time::timeout_at(deadline, data_socket.next())
            .await
            .expect("the tape flows before the blackout")
            .expect("open")
            .expect("frame");
        if matches!(message, Message::Text(ref text)
            if matches!(serde_json::from_str(text), Ok(ServerMessage::Trade(_))))
        {
            break;
        }
    }

    // Arm a blackout over the control plane. It is armed against the RUN, not
    // against an account, so it must gate this socket's market data.
    let (_, arm_clock_body) = http_get(&venue.http_base(), "/clock");
    let arm_clock: serde_json::Value = serde_json::from_str(&arm_clock_body).expect("arm clock");
    let armed_at = arm_clock["server_now_ns"].as_u64().expect("arm instant");
    let armed = post_divergence(&venue.http_base(), r#"{"type":"StallData","ms":180000}"#);
    assert_eq!(armed, 202, "the divergence is accepted");

    // Within the window no market data may arrive on this socket.
    let quiet_until = tokio::time::Instant::now() + Duration::from_secs(2);
    while let Ok(Some(Ok(message))) = tokio::time::timeout_at(quiet_until, data_socket.next()).await
    {
        if let Message::Text(text) = message {
            let event_ts = match serde_json::from_str::<ServerMessage>(&text) {
                Ok(ServerMessage::Trade(trade)) => Some(trade.ts_event),
                Ok(ServerMessage::Quote(quote)) => Some(quote.ts_event),
                _ => None,
            };
            assert!(
                event_ts.is_none_or(|ts| ts <= armed_at),
                "market data generated after an armed StallData window arrived"
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
/// ITS PREMISE IS NOT ENFORCED BY CONSTRUCTION and is asserted rather than
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
    let sim_now = clock["server_now_ns"].as_u64().expect("sim now");
    let (_, trades_body) = http_get(
        &venue.http_base(),
        &format!(
            "/trades?symbol={}&start={}&end={sim_now}&limit=10000",
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

    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
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
        match serde_json::from_str::<ServerMessage>(&text) {
            Ok(ServerMessage::OrderAccepted { ts_event, .. }) => accepted_ts = Some(ts_event),
            Ok(ServerMessage::OrderFilled(fill)) => {
                assert_eq!(fill.client_order_id, "BAND-1");
                // The LIQUIDITY SIDE, not the timestamp, is what names the case.
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
            Ok(ServerMessage::OrderRejected { reason, .. }) => {
                panic!("the banded limit was rejected: {reason}")
            }
            // Named rather than swallowed by a wildcard: each of these three
            // says the venue never got a chance to do what the test is asking
            // about, and under a wildcard the test would instead die on the
            // 60-second deadline blaming the sweep.
            Ok(ServerMessage::AdmissionRejected {
                subject, reason, ..
            }) => panic!("the venue refused admission for {subject:?}: {reason}"),
            Ok(ServerMessage::ProtocolError { reason, .. }) => {
                panic!("the venue read the submit as malformed: {reason}")
            }
            Ok(ServerMessage::FeedLagged { skipped, .. }) => {
                panic!("this socket dropped {skipped} frames, so the fill may never arrive")
            }
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
/// Proven by the fill PRICE: a market order priced absurdly far from the market
/// fills near the tape rather than at its own number, which is only possible if
/// the venue read the tape. Adverse-or-equal is asserted on the tape-priced
/// fill in the same breath.
///
/// Retried, because `read_market` legitimately REFUSES at any instant whose
/// trailing window carries fewer than `MIN_VOL_SAMPLES` returns, and the fitted
/// BTCUSDT tape does that at a substantial fraction of instants. A refused
/// reading is the documented fallback - stated price, no slippage, WARN - so a
/// single attempt would be a coin flip. What is pinned is that a reading IS
/// taken on both paths: every attempt filling at its own number would mean the
/// path never reads at all, which is the defect this test exists for.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn a_market_submit_takes_a_reading_on_both_the_priced_and_priceless_paths() {
    const ATTEMPTS: usize = 8;
    let venue = spawn(&["--config", &band_config()]);
    let (mut socket, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("open the order socket");

    for path in ["priced", "priceless"] {
        let price = if path == "priced" {
            r#","price":"9000000""#
        } else {
            ""
        };
        let mut read_the_tape = false;
        for attempt in 0..ATTEMPTS {
            let id = format!("MKT-{path}-{attempt}");
            let submit = format!(
                r#"{{"type":"SubmitOrder","client_order_id":"{id}","symbol":"{}","side":"Buy","order_type":"Market","quantity":"0.01"{price},"time_in_force":"Gtc"}}"#,
                venue.symbol
            );
            socket
                .send(Message::Text(submit.into()))
                .await
                .expect("submit the market order");

            let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
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
                match serde_json::from_str::<ServerMessage>(&text) {
                    Ok(ServerMessage::OrderAccepted {
                        client_order_id,
                        ts_event,
                        ..
                    }) if client_order_id == id => accepted_ts = Some(ts_event),
                    Ok(ServerMessage::OrderFilled(fill)) if fill.client_order_id == id => {
                        break fill;
                    }
                    Ok(ServerMessage::OrderRejected { reason, .. }) => {
                        panic!("{id} was rejected: {reason}")
                    }
                    _ => {}
                }
            };

            // The venue decides a market submit against `MarketReadingCache`,
            // which memoizes `read_market` on the SWEEP-INTERVAL bucket (10 ms
            // under band.toml). The reading therefore names the last print at or
            // before the start of the acceptance instant's bucket, and the
            // adverse-slippage invariant has to be asserted against THAT print,
            // not against the last print at the fill instant - the tape moves on
            // between the reading and the fill, and a buy filling below a print
            // it could not have seen is not favourable slippage.
            //
            // Worse, the reading instant is not the acceptance instant either:
            // it is whenever the submit reached the handler, which at speed 100
            // can be many sim seconds earlier. So the print the venue read
            // cannot be identified from outside - only BRACKETED. What is
            // asserted is therefore the strongest statement that survives the
            // bracket: the reading's print lies somewhere in the lookback below,
            // and the fill band is adverse, so a market BUY must fill at or
            // above the LOWEST price in that window. A fill decided off no
            // reading at all - the defect this test exists for - breaks it.
            // Recovering the exact per-fill statement means putting the reading
            // instant on the `OrderFilled` event or dropping the cache's
            // bucketing; neither is this test's call to make.
            const BUCKET_NS: u64 = 10_000_000;
            let reading_ts = accepted_ts.unwrap_or(fill.ts_event) / BUCKET_NS * BUCKET_NS;
            let (_, body) = http_get(
                &venue.http_base(),
                &format!(
                    "/trades?symbol={}&start={}&end={}&limit=10000",
                    venue.symbol,
                    // A 300 s lookback, not a 1 s one: the arrival clock's quiet
                    // state runs a mean gap of several seconds, so a one-second
                    // window is legitimately empty often enough to make this
                    // test flaky for a reason that has nothing to do with fills.
                    reading_ts.saturating_sub(300_000_000_000),
                    reading_ts
                ),
            );
            let trades: Vec<TradeTick> =
                serde_json::from_str(&body).expect("tape at the reading instant");
            let last = trades
                .last()
                .expect("a print at or before the reading")
                .price;
            let floor = trades
                .iter()
                .map(|trade| trade.price)
                .min()
                .expect("a print in the lookback");
            if fill.last_px < last * rust_decimal::Decimal::TWO {
                read_the_tape = true;
                assert!(
                    fill.last_px >= floor,
                    "a market buy filled below every print it could have read: {} < {floor}",
                    fill.last_px
                );
                break;
            }
            // The refusal fallback fired. Let the clock move so the next
            // attempt reads a genuinely different window - at speed 100 this is
            // fifty sim seconds of fresh tape.
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        assert!(
            read_the_tape,
            "the {path} market path never took a tape reading in {ATTEMPTS} attempts"
        );
    }
}

#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn the_tape_is_identical_with_and_without_order_flow() {
    let venue = spawn(&["--config", &band_config()]);
    let path = format!(
        "/trades?symbol={}&start={}&limit=200",
        venue.symbol, venue.record.data_origin_ns
    );
    let (status, before) = http_get(&venue.http_base(), &path);
    assert_eq!(status, 200);
    let (mut socket, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("open order socket");
    for index in 0..100 {
        let submit = format!(
            r#"{{"type":"SubmitOrder","client_order_id":"TAPE-{index}","symbol":"{}","side":"Buy","order_type":"Limit","quantity":"0.01","price":"1","time_in_force":"Gtc"}}"#,
            venue.symbol
        );
        socket
            .send(Message::Text(submit.into()))
            .await
            .expect("submit");
    }
    // Drain to the LAST acceptance before re-reading. Comparing the pages while
    // the submits were still in flight would let a clean run and a broken one
    // look alike.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let message = tokio::time::timeout_at(deadline, socket.next())
            .await
            .expect("the venue answered every submit")
            .expect("the socket stayed open")
            .expect("a websocket frame");
        let Message::Text(text) = message else {
            continue;
        };
        if let Ok(ServerMessage::OrderAccepted {
            client_order_id, ..
        }) = serde_json::from_str::<ServerMessage>(&text)
            && client_order_id == "TAPE-99"
        {
            break;
        }
    }
    let (status, after) = http_get(&venue.http_base(), &path);
    assert_eq!(status, 200);
    assert_eq!(
        before, after,
        "client order flow advanced or altered the clean tape"
    );
}

/// The tape-purity property, extended to conditionals: no client conditional
/// advances any generator state.
///
/// A resting conditional is the one order shape that puts a SECOND kind of scan
/// into the sweeper's per-symbol walk (`ScanKind::TriggerTouch` beside the
/// limits' `FillThrough`), and the walk drains the tape source. If a trigger
/// scan drained prints the canonical `/trades` page would otherwise have served,
/// or advanced the generator past them, the two reads of the same fixed window
/// would differ. They must not.
///
/// It lives at the SERVER layer rather than in `mogwai-engine`, where the spec's
/// gate list names it: the engine holds no tape and no generator, so the
/// property it asserts is only expressible where the walk and the source
/// actually are. Its twin above is the same assertion for plain limits.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn the_tape_is_identical_with_and_without_a_resting_stop() {
    let venue = spawn(&["--config", &band_config()]);
    let path = format!(
        "/trades?symbol={}&start={}&limit=200",
        venue.symbol, venue.record.data_origin_ns
    );
    let (status, before) = http_get(&venue.http_base(), &path);
    assert_eq!(status, 200);
    let (mut socket, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("open order socket");
    // Sell stops at a trigger of 1: unreachable by any BTCUSDT print, so every
    // one of them REMAINS resting and untriggered for the whole test, which is
    // the state that puts a touch scan into every sweep pass. Half stop-market
    // and half stop-limit, because the two rest identically as
    // `Resting::Conditional` but reach the walk through different submit paths.
    // They are `reduce_only`, which is what a protective leg on a flat book
    // actually is: the funded account holds no BTC, and section 1.8's admission
    // exemption is precisely what lets such a leg rest rather than be refused at
    // the door.
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
        socket
            .send(Message::Text(submit.into()))
            .await
            .expect("submit");
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let message = tokio::time::timeout_at(deadline, socket.next())
            .await
            .expect("the venue answered every submit")
            .expect("the socket stayed open")
            .expect("a websocket frame");
        let Message::Text(text) = message else {
            continue;
        };
        match serde_json::from_str::<ServerMessage>(&text) {
            Ok(ServerMessage::OrderRejected {
                client_order_id,
                reason,
                ..
            }) => panic!("{client_order_id} was rejected: {reason}"),
            Ok(ServerMessage::OrderTriggered {
                client_order_id, ..
            }) => panic!("{client_order_id} triggered: a trigger of 1 is unreachable"),
            Ok(ServerMessage::OrderAccepted {
                client_order_id, ..
            }) if client_order_id == "STOP-99" => break,
            _ => {}
        }
    }
    // Let several sweep passes run WITH the hundred touch scans in the book.
    // Draining to the last acceptance only proves the submit path is pure; the
    // walk is what this test is actually about, and it runs on its own cadence
    // (`fill_sweep_interval_ms = 10`).
    tokio::time::sleep(Duration::from_millis(500)).await;

    let (status, after) = http_get(&venue.http_base(), &path);
    assert_eq!(status, 200);
    assert_eq!(
        before, after,
        "a resting conditional's touch scan advanced or altered the clean tape"
    );
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

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
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
        match serde_json::from_str::<ServerMessage>(&text) {
            Ok(ServerMessage::OrderAccepted {
                client_order_id, ..
            }) if client_order_id == "ORDERED-1" => accepted = true,
            Ok(ServerMessage::OrderCanceled {
                client_order_id, ..
            }) if client_order_id == "ORDERED-1" => {
                assert!(accepted, "cancel completed before its submit");
                break;
            }
            Ok(ServerMessage::OrderCancelRejected {
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

#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn websocket_command_work_is_bounded_without_an_act_delay() {
    let config = format!(
        "{}/tests/configs/command-cap.toml",
        env!("CARGO_MANIFEST_DIR")
    );
    let venue = spawn(&["--config", &config]);
    let (mut socket, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("open order socket");
    for index in 0..50 {
        let command = format!(
            r#"{{"type":"SubmitOrder","client_order_id":"CAP-{index}","symbol":"{}","side":"Buy","order_type":"Market","quantity":"0.01","time_in_force":"Gtc"}}"#,
            venue.symbol
        );
        socket
            .send(Message::Text(command.into()))
            .await
            .expect("send");
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let message = tokio::time::timeout_at(deadline, socket.next())
            .await
            .expect("capacity refusal before deadline")
            .expect("socket remains open")
            .expect("valid websocket frame");
        let Message::Text(text) = message else {
            continue;
        };
        if matches!(
            serde_json::from_str::<ServerMessage>(&text),
            Ok(ServerMessage::AdmissionRejected { ref reason, .. })
                if reason == "venue command capacity exhausted"
        ) {
            break;
        }
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
            "x".repeat(mogwai_protocol::MAX_CLIENT_MESSAGE_BYTES + 1)
                .into(),
        ))
        .await
        .expect("send oversized message");
    // The socket is a LIVE market-data feed, so frames the venue had already
    // written when the oversized frame landed are still in flight and arrive
    // first: the assertion is that the connection ENDS, not that the close is
    // the very next frame. It must still end promptly, so the deadline is the
    // real assertion - a venue that kept serving this connection indefinitely
    // would time out here rather than quietly pass.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut ended = false;
    while !ended {
        let message = tokio::time::timeout_at(deadline, socket.next())
            .await
            .expect("the oversized frame ends the connection before the deadline");
        match message {
            Some(Ok(Message::Close(_)) | Err(_)) | None => ended = true,
            Some(Ok(Message::Text(text))) => {
                // Market data may precede the close. What must never appear is
                // a protocol-level ANSWER to the oversized input: that would
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

/// One `POST /control/divergence`, returning the status code AND the body,
/// because a refusal that must NAME something is only half asserted by its
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

/// Generator havoc forks the river, so it belongs to the sharing key at
/// placement and may not mutate water a boat is already sitting on. The boot
/// river always carries the boot boat, so this is the refusal an operator sees.
#[test]
#[ignore = "binds a loopback listener"]
fn a_generator_arm_on_a_boated_river_is_refused_naming_the_forking_alternative() {
    let venue = spawn(&["--config", &fast_config()]);
    let symbol = venue.symbol.clone();
    let (status, body) = post_divergence_body(
        &venue.http_base(),
        &format!(
            r#"{{"type":"FlowSurge","symbol":"{symbol}","rate_mult":2.0,"children_mult":2.0,"duration_ms":1000}}"#
        ),
    );
    assert_eq!(
        status, 400,
        "a seated river refuses a generator arm: {body}"
    );
    assert!(
        body.contains(&*symbol),
        "the refusal names the river: {body}"
    );
    assert!(
        body.contains("sharing key"),
        "the refusal names the forking alternative: {body}"
    );
}

/// Unqualified, a generator arm does NOT fan out over every river. It is
/// refused while any boat is seated, and the refusal names those rivers.
#[test]
#[ignore = "binds a loopback listener"]
fn a_generator_arm_with_no_symbol_is_refused_naming_the_boated_rivers() {
    let venue = spawn(&["--config", &fast_config()]);
    let (status, body) = post_divergence_body(
        &venue.http_base(),
        r#"{"type":"FlowSurge","rate_mult":2.0,"children_mult":2.0,"duration_ms":1000}"#,
    );
    assert_eq!(
        status, 400,
        "an unqualified generator arm is refused: {body}"
    );
    assert!(
        body.contains(&*venue.symbol),
        "the refusal names the seated river: {body}"
    );
}

/// A river with no boat takes the arm: nothing straddles it, so history and
/// every later passenger see the same surged water.
#[test]
#[ignore = "binds a loopback listener"]
fn a_generator_arm_on_an_unboated_river_is_accepted() {
    let venue = spawn(&["--config", &two_symbols_config()]);
    let (status, body) = post_divergence_body(
        &venue.http_base(),
        r#"{"type":"FlowSurge","symbol":"MNQ","rate_mult":2.0,"children_mult":2.0,"duration_ms":1000}"#,
    );
    assert_eq!(status, 202, "an unboated river accepts the arm: {body}");
    // The ack names the armed span, because an operator otherwise cannot see
    // WHAT was armed against WHICH origin: the window opens at the river's
    // origin, so the boat placed afterwards gets the whole span rather than a
    // window already closed in its past.
    assert!(
        body.contains("1000") && body.contains("MNQ"),
        "the ack names the armed span and river: {body}"
    );
}

/// `CancelOpenOrderSilently` takes its clock from the TARGETED ORDER: the id
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
    // drained for, never asserted on as the NEXT frame.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
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

/// `/clock` answers for the named river's boat, and LABELS the venue-clock
/// fallback so a caller cannot read it as a boat's own time.
#[test]
#[ignore = "binds a loopback listener"]
fn clock_answers_per_boat_when_a_symbol_is_named() {
    let venue = spawn(&["--config", &two_symbols_config()]);
    let (status, boated) = http_get(
        &venue.http_base(),
        &format!("/clock?symbol={}", venue.symbol),
    );
    assert_eq!(status, 200, "the boot river answers: {boated}");
    let boated: mogwai_protocol::ServerClock = serde_json::from_str(&boated).unwrap();
    assert!(boated.boat_clock, "the boot river carries a boat");

    let (status, unboated) = http_get(&venue.http_base(), "/clock?symbol=MNQ");
    assert_eq!(status, 200, "an unboated river still answers: {unboated}");
    let unboated: mogwai_protocol::ServerClock = serde_json::from_str(&unboated).unwrap();
    assert!(
        !unboated.boat_clock,
        "the venue-clock fallback is labelled as such"
    );

    let (status, unnamed) = http_get(&venue.http_base(), "/clock");
    assert_eq!(status, 200, "an unnamed clock still answers: {unnamed}");
    let unnamed: mogwai_protocol::ServerClock = serde_json::from_str(&unnamed).unwrap();
    assert!(!unnamed.boat_clock, "no symbol names no boat");
}

/// A duration is a property of the PASSENGER. One passenger's deadline closes
/// its own socket and leaves the boat carrying everyone else.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn a_passenger_duration_closes_one_socket_and_leaves_the_boat_running() {
    let venue = spawn(&["--config", &fast_config()]);
    let (mut staying, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("the indefinite passenger boards");
    let (mut leaving, _) = tokio_tungstenite::connect_async(format!(
        "{}?symbol={}&duration_ms=1500",
        venue.ws_url(),
        venue.symbol
    ))
    .await
    .expect("the bounded passenger boards the same boat");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut announced = false;
    loop {
        let message = tokio::time::timeout_at(deadline, leaving.next())
            .await
            .expect("the bounded passenger closes before the deadline");
        match message {
            Some(Ok(Message::Text(text))) => announced |= text.contains("RunComplete"),
            Some(Ok(Message::Close(_)) | Err(_)) | None => break,
            Some(Ok(_)) => {}
        }
    }
    assert!(
        announced,
        "the bounded passenger announced its completion before closing"
    );

    // The boat is still carrying the other passenger, so frames keep arriving
    // on a socket that asked for no duration at all.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while let Ok(Some(Ok(message))) = tokio::time::timeout_at(deadline, staying.next()).await {
        if matches!(message, Message::Text(_)) {
            return;
        }
    }
    panic!("one passenger's deadline wound down the boat under another");
}
