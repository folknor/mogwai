// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! L2 gates: the declared run duration, the announced completion, and the
//! deliberate difference between finishing and being killed.
//!
//! `RunComplete` exists so a client can tell a finished run from a dead one.
//! These tests are therefore as much about what is NOT sent under a signal as
//! about what is sent at the deadline.

mod common;

use std::time::Duration;

use common::{Venue, accelerated_config, fast_config, spawn};
use futures_util::StreamExt;
use mogwai_protocol::ServerMessage;
use tokio_tungstenite::tungstenite::Message;

/// Opens a socket onto the run and returns the stream.
async fn connect(
    venue: &Venue,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let (socket, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("open a socket onto the run");
    socket
}

/// Drains until a `RunComplete` arrives, then returns it together with whether
/// the peer closed afterwards. `None` means the socket ended without one.
async fn drain_to_completion(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    timeout: Duration,
) -> (Option<(u64, u64)>, bool) {
    let mut completion = None;
    let mut closed = false;
    let deadline = tokio::time::Instant::now() + timeout;
    while let Ok(Some(Ok(message))) = tokio::time::timeout_at(deadline, socket.next()).await {
        match message {
            Message::Text(text) => {
                if let Ok(ServerMessage::RunComplete {
                    sim_now_ns,
                    elapsed_ns,
                }) = serde_json::from_str::<ServerMessage>(&text)
                {
                    completion = Some((sim_now_ns, elapsed_ns));
                }
            }
            Message::Close(frame) => {
                closed = true;
                if let Some(frame) = frame {
                    assert_eq!(
                        u16::from(frame.code),
                        1000,
                        "a completed run closes with WS 1000, not a fault code"
                    );
                }
                break;
            }
            _ => {}
        }
    }
    (completion, closed)
}

/// The declared duration is a contract: the venue serves exactly that much sim
/// time, says so, and exits 0 on its own without anybody signalling it.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn venue_announces_run_complete_and_exits_zero_at_the_declared_sim_deadline() {
    let mut venue = spawn(&["--config", &fast_config(), "--duration", "2s"]);
    assert_eq!(
        venue.record.run_duration_ns,
        Some(2_000_000_000),
        "the readiness record reports the declared duration"
    );

    let mut socket = connect(&venue).await;
    let (completion, closed) = drain_to_completion(&mut socket, Duration::from_secs(30)).await;

    let (_, elapsed_ns) = completion.expect("the run announces its completion on the wire");
    assert!(
        elapsed_ns >= 2_000_000_000,
        "the run served {elapsed_ns} ns of a declared 2s"
    );
    assert!(closed, "the announcement is followed by a close");

    let status = venue.wait_for_exit(Duration::from_secs(20));
    assert_eq!(
        status.code,
        Some(0),
        "a planned completion is exit 0, not a crash"
    );
}

/// The `watch` fanout is the thing under test: with no registry of connections,
/// every open socket must still see the announcement.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn run_complete_reaches_every_open_socket() {
    let mut venue = spawn(&["--config", &fast_config(), "--duration", "2s"]);

    let mut first = connect(&venue).await;
    let mut second = connect(&venue).await;

    let (left, right) = tokio::join!(
        drain_to_completion(&mut first, Duration::from_secs(30)),
        drain_to_completion(&mut second, Duration::from_secs(30)),
    );

    assert!(
        left.0.is_some(),
        "the first socket saw the completion announcement"
    );
    assert!(
        right.0.is_some(),
        "the second socket saw it too - the fanout reaches every connection"
    );
    assert!(left.1 && right.1, "both sockets were closed with WS 1000");

    assert_eq!(venue.wait_for_exit(Duration::from_secs(20)).code, Some(0));
}

/// A signal is not a planned completion and must not be reported as one. This
/// is the whole reason `RunComplete` exists, so it is pinned directly.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn sigterm_closes_without_announcing_run_complete() {
    let mut venue = spawn(&["--config", &fast_config()]);
    let mut socket = connect(&venue).await;

    // Let the connection settle so a missed frame cannot be blamed on a race
    // between the upgrade and the signal.
    tokio::time::sleep(Duration::from_millis(300)).await;
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(venue.record.pid).expect("pid fits")),
        nix::sys::signal::Signal::SIGTERM,
    )
    .expect("signal the venue");

    let (completion, _) = drain_to_completion(&mut socket, Duration::from_secs(20)).await;
    assert!(
        completion.is_none(),
        "a signalled venue announced a planned completion it never had: {completion:?}"
    );
    venue.wait_for_exit(Duration::from_secs(20));
}

/// Decision 8. A large warmup, a fast clock and a short declared duration: if
/// the deadline were measured from boot instead of from the post-warmup
/// `started_ns`, the accelerated duration would elapse DURING warmup and the
/// run would be over before the launcher could connect. It must instead serve
/// its whole declared duration after readiness.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn a_short_accelerated_run_is_not_over_before_it_is_ready() {
    let mut venue = spawn(&["--config", &accelerated_config(), "--duration", "30s"]);

    // The run start is the post-warmup instant, so the whole declared warmup
    // sits behind it on the sim axis.
    assert_eq!(
        venue.record.run_start_ns - venue.record.data_origin_ns,
        venue.record.warmup_ns
    );

    let mut socket = connect(&venue).await;
    let (completion, _) = drain_to_completion(&mut socket, Duration::from_secs(60)).await;
    let (sim_now_ns, elapsed_ns) =
        completion.expect("the run was still serving when the launcher connected");

    assert!(
        elapsed_ns >= 30_000_000_000,
        "the run served only {elapsed_ns} ns of a declared 30s; the deadline epoch \
         is boot rather than the post-warmup start"
    );
    assert!(
        sim_now_ns >= venue.record.run_start_ns + 30_000_000_000,
        "sim-now at completion is at least the declared duration past run_start_ns"
    );
    assert_eq!(venue.wait_for_exit(Duration::from_secs(20)).code, Some(0));
}
