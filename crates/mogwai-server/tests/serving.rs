// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! L3-L6 gates: the history floor, the eagerly materialized warmup, the
//! subscription-free feed, and the single ledger every connection shares.

mod common;

use std::time::Duration;

use common::{band_config, fast_config, http_get, paced_config, spawn, tiny_fanout_config};
use futures_util::{SinkExt, StreamExt};
use mogwai_protocol::{ServerMessage, TradeTick};
use tokio_tungstenite::tungstenite::Message;

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
            venue.record.symbol, venue.record.run_start_ns
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
        &format!("/trades?symbol={}&start={far_future}", venue.record.symbol),
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
            venue.record.symbol, venue.record.run_start_ns
        ),
    );
    assert_eq!(status, 200, "a future end is clamped, not refused: {body}");
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
        &format!(
            "/trades?symbol={}&start={floor}&limit=50",
            venue.record.symbol
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
            assert_eq!(trade.symbol, venue.record.symbol);
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
    let mut lagged = None;
    let mut close_code = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
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
        venue.record.symbol
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
    let armed = post_divergence(&venue.http_base(), r#"{"type":"StallData","ms":180000}"#);
    assert_eq!(armed, 202, "the divergence is accepted");

    // Within the window no market data may arrive on this socket.
    let quiet_until = tokio::time::Instant::now() + Duration::from_secs(2);
    while let Ok(Some(Ok(message))) = tokio::time::timeout_at(quiet_until, data_socket.next()).await
    {
        if let Message::Text(text) = message
            && matches!(
                serde_json::from_str::<ServerMessage>(&text),
                Ok(ServerMessage::Trade(_) | ServerMessage::Quote(_))
            )
        {
            panic!(
                "market data arrived during an armed StallData window; the \
                 divergence was armed somewhere this connection cannot see"
            );
        }
    }
}

/// The run owns the fill sweep now that no account does. Without a sweep task
/// on the run, a banded limit rests forever: a submit decides only its own
/// order, against the reading it arrived with, so nothing else ever walks the
/// span its trigger waits on, and the venue would be accepting orders it can
/// never execute.
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
            venue.record.symbol,
            sim_now.saturating_sub(300_000_000_000)
        ),
    );
    let trades: Vec<TradeTick> = serde_json::from_str(&trades_body).expect("anchor trades");
    let price = trades.last().expect("anchor print").price;
    let submit = format!(
        r#"{{"type":"SubmitOrder","client_order_id":"BAND-1","symbol":"{}","side":"Buy","order_type":"Limit","quantity":"0.01","price":"{price}","time_in_force":"Gtc"}}"#,
        venue.record.symbol,
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
                assert!(
                    accepted_ts.is_some_and(|accepted| fill.ts_event > accepted),
                    "a banded limit must rest before the sweep fills it"
                );
                assert_eq!(fill.client_order_id, "BAND-1");
                return;
            }
            Ok(ServerMessage::OrderRejected { reason, .. }) => {
                panic!("the banded limit was rejected: {reason}")
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
                venue.record.symbol
            );
            socket
                .send(Message::Text(submit.into()))
                .await
                .expect("submit the market order");

            let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
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
                    Ok(ServerMessage::OrderFilled(fill)) if fill.client_order_id == id => {
                        break fill;
                    }
                    Ok(ServerMessage::OrderRejected { reason, .. }) => {
                        panic!("{id} was rejected: {reason}")
                    }
                    _ => {}
                }
            };

            let (_, body) = http_get(
                &venue.http_base(),
                &format!(
                    "/trades?symbol={}&start={}&end={}&limit=10000",
                    venue.record.symbol,
                    fill.ts_event.saturating_sub(300_000_000_000),
                    fill.ts_event
                ),
            );
            let trades: Vec<TradeTick> =
                serde_json::from_str(&body).expect("tape at the fill instant");
            let last = trades.last().expect("a print at or before the fill").price;
            if fill.last_px < last * rust_decimal::Decimal::TWO {
                read_the_tape = true;
                assert!(
                    fill.last_px >= last,
                    "a market buy filled better than the market: {} < {last}",
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
        venue.record.symbol, venue.record.data_origin_ns
    );
    let (status, before) = http_get(&venue.http_base(), &path);
    assert_eq!(status, 200);
    let (mut socket, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("open order socket");
    for index in 0..100 {
        let submit = format!(
            r#"{{"type":"SubmitOrder","client_order_id":"TAPE-{index}","symbol":"{}","side":"Buy","order_type":"Limit","quantity":"0.01","price":"1","time_in_force":"Gtc"}}"#,
            venue.record.symbol
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
        venue.record.symbol, venue.record.data_origin_ns
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
                venue.record.symbol
            )
        } else {
            format!(
                r#"{{"type":"SubmitOrder","client_order_id":"STOP-{index}","symbol":"{}","side":"Sell","order_type":"StopLimit","quantity":"0.01","price":"1","trigger_price":"1","reduce_only":true,"time_in_force":"Gtc"}}"#,
                venue.record.symbol
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
