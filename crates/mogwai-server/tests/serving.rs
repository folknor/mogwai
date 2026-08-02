// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! L3-L6 gates: the history floor, the eagerly materialized warmup, the
//! subscription-free feed, and the single ledger every connection shares.

mod common;

use std::time::Duration;

use common::{fast_config, gated_config, http_get, paced_config, spawn, tiny_fanout_config};
use futures_util::{SinkExt, StreamExt};
use mogwai_protocol::{ServerMessage, TradeTick};
use tokio_tungstenite::tungstenite::Message;

/// The silent failure the eager-warmup ruling exists to remove: an off-tape
/// window used to drain the seek budget and come back an empty `200` the caller
/// could not tell from "no trades happened". It is now a named refusal.
#[test]
#[ignore = "binds a loopback listener"]
fn trades_before_the_history_floor_are_refused_with_400() {
    let venue = spawn(&["--config", &fast_config()]);
    let floor = venue.record.data_origin_ns;

    let (status, body) = http_get(
        &venue.http_base(),
        &format!("/trades?symbol={}&start={}", venue.record.symbol, floor - 1),
    );
    assert_eq!(
        status, 400,
        "an off-tape start is refused, not served short"
    );
    assert!(
        body.contains(&floor.to_string()),
        "the refusal names the earliest servable instant: {body}"
    );
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
    let armed = post_divergence(&venue.http_base(), r#"{"type":"StallData","ms":60000}"#);
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
/// on the run, a penetration-gated limit rests forever: a submit seeds only its
/// own order, so nothing else ever advances a penetration count, and the venue
/// would be accepting orders it can never execute - which is exactly what
/// `validate_penetration` refuses to ship.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn a_gated_limit_fills_from_the_run_sweep() {
    let venue = spawn(&["--config", &gated_config()]);
    let (mut socket, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("open the order socket");

    // A buy the whole tape prints through, so every print counts against the
    // gate. The acceptance reading seeds one penetration of the two required,
    // so it must REST on submit and be filled by a later sweep pass.
    let submit = format!(
        r#"{{"type":"SubmitOrder","client_order_id":"GATED-1","symbol":"{}","side":"Buy","order_type":"Limit","quantity":"0.01","price":"1000000","time_in_force":"Gtc"}}"#,
        venue.record.symbol
    );
    socket
        .send(Message::Text(submit.into()))
        .await
        .expect("submit the gated limit");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let mut accepted = false;
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
            Ok(ServerMessage::OrderAccepted { .. }) => accepted = true,
            Ok(ServerMessage::OrderFilled(fill)) => {
                assert!(
                    accepted,
                    "a gated limit must be accepted and rest before it fills"
                );
                assert_eq!(fill.client_order_id, "GATED-1");
                return;
            }
            Ok(ServerMessage::OrderRejected { reason, .. }) => {
                panic!("the gated limit was rejected: {reason}")
            }
            _ => {}
        }
    }
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
