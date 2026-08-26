// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Piece 13's instrument: servable but unconfigured.
//!
//! No other harness can observe it. Every existing gate names a symbol some
//! config declared, so all of them pass whether resolution is total or boot
//! fixed - only a run asked for a label nobody configured tells the two apart.
//!
//! Two properties, one per case: a socket bind serves an unconfigured label and
//! makes it advertise, and a history poll alone does the same. Both spend a
//! river, and the advertised set is the materialized set.

mod common;

use std::time::Duration;

use common::{fast_config, http_get, spawn};
use futures_util::StreamExt;
use mogwai_protocol::InstrumentDef;
use tokio_tungstenite::tungstenite::Message;

/// A label no config in this tree mentions, wire-legal and not a preset name,
/// so it resolves to the default bundle wearing its own label.
///
/// `FOOBAR` is the workspace's idiom for exactly this - `config.rs`, `source.rs`
/// and `seeds.rs` all use it as the unconfigured placeholder, and
/// `configs/unmatched-symbol.toml` boots a venue on it - so it is deliberately
/// not renamed to something locally unique. The literal is not what makes the
/// tests below fragile. What makes them fragile is that each one asserts an
/// absence first: `!advertises(..)` is a statement about a venue nothing has
/// materialized this label on yet, so it is sound only while the venue is this
/// test's alone. Both tests here must therefore stay on the owned side of any
/// shared-venue split; a shared `fast.toml` venue would let another test spend
/// the river first and this would fail on a run where nothing is wrong.
const UNCONFIGURED: &str = "FOOBAR";

fn instruments(base: &str) -> Vec<InstrumentDef> {
    let (status, body) = http_get(base, "/instruments");
    assert_eq!(status, 200, "instrument list answers: {body}");
    serde_json::from_str(&body).unwrap_or_else(|err| panic!("{body} is not a list: {err}"))
}

fn advertises(base: &str, symbol: &str) -> bool {
    instruments(base)
        .iter()
        .any(|def| def.symbol.as_ref() == symbol)
}

#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn a_run_serves_a_symbol_nobody_configured() {
    let venue = spawn(&["--config", &fast_config()]);
    let base = venue.http_base();

    // Absent first, and deliberately so: resolution is total, but nothing has
    // materialized this label yet. A later poll or bind would - and does below.
    assert!(
        !advertises(&base, UNCONFIGURED),
        "a shape nothing has materialized must not advertise"
    );

    let (mut socket, response) = tokio_tungstenite::connect_async(venue.ws_url_for(UNCONFIGURED))
        .await
        .expect("an unconfigured label binds a socket");
    assert_eq!(response.status(), 101);

    // Drain to a deadline, never asserting on the next frame: every socket is
    // attached to the live tape on upgrade, so what arrives first is whatever
    // the venue was emitting.
    // Clamped to the test's wall budget: a 30 s bound sits past the 20 s
    // per-test hang watchdog, so the "no market frame" panic below could never
    // have been printed - the watchdog would have killed the process group
    // first, naming the test and nothing else. This file was not part of the
    // sweep that closed that class in the other three.
    //
    // And it records how the stream ended. The shape this replaces -
    // `while let Ok(Some(Ok(Message::Text(_))))` - exits on a Ping, a Binary or
    // a Close as well as on the deadline, and every one of them then arrived as
    // "the bound river produced no market frame". That is the wrong answer rather
    // than a timeout: a venue that closed the socket, or that sent a control
    // frame before its first print, would have been reported as a venue that
    // served an unlabelled river.
    let deadline = common::deadline(Duration::from_secs(10));
    let mut labelled = false;
    let ending = loop {
        match tokio::time::timeout_at(deadline, socket.next()).await {
            Err(_) => break "the deadline expired with no labelled market frame".to_string(),
            Ok(None) => break "the venue ended the stream".to_string(),
            Ok(Some(Err(err))) => break format!("the socket failed: {err}"),
            Ok(Some(Ok(Message::Close(frame)))) => {
                break format!("the venue closed the socket: {frame:?}");
            }
            Ok(Some(Ok(Message::Text(frame)))) => {
                if frame.contains(UNCONFIGURED)
                    && (frame.contains("\"Trade\"") || frame.contains("\"Quote\""))
                {
                    labelled = true;
                    break String::new();
                }
            }
            // A Ping, a Pong or a Binary frame is not the end of anything.
            Ok(Some(Ok(_))) => {}
        }
    };
    assert!(
        labelled,
        "the bound river produced no market frame labelled {UNCONFIGURED}: {ending}"
    );

    let defs = instruments(&base);
    let served = defs
        .iter()
        .find(|def| def.symbol.as_ref() == UNCONFIGURED)
        .unwrap_or_else(|| panic!("a bound label must advertise: {defs:?}"));
    let default = defs
        .iter()
        .find(|def| def.symbol.as_ref() == venue.symbol)
        .expect("the boot shape advertises");
    assert_eq!(
        served.class.settlement_currency(),
        default.class.settlement_currency(),
        "the default bundle is the SHAPE CONTRACT for an unnamed symbol"
    );
    assert_eq!(served.price_increment, default.price_increment);

    let (status, body) = http_get(
        &base,
        &format!("/operator/trades?symbol={UNCONFIGURED}&start=0&limit=5"),
    );
    assert_eq!(status, 200, "its history is servable too: {body}");
    assert!(body.contains(UNCONFIGURED), "{body}");
}

/// Ruling 3's other half: a poll materializes, so a poll advertises. Nothing
/// binds a socket here.
#[test]
#[ignore = "binds a loopback listener"]
fn a_history_poll_alone_materializes_and_advertises() {
    let venue = spawn(&["--config", &fast_config()]);
    let base = venue.http_base();
    const POLLED: &str = "BARFOO";

    assert!(!advertises(&base, POLLED));
    let (status, body) = http_get(
        &base,
        &format!("/operator/trades?symbol={POLLED}&start=0&limit=5"),
    );
    assert_eq!(status, 200, "an unconfigured poll is served: {body}");
    assert!(
        advertises(&base, POLLED),
        "a poll spends a river, and the advertised set is the materialized set"
    );
}
