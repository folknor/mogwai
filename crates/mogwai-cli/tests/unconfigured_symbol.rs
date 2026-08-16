// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Piece 13's instrument: SERVABLE BUT UNCONFIGURED.
//!
//! No other harness can observe it. Every existing gate names a symbol some
//! config declared, so all of them pass whether resolution is total or boot
//! fixed - only a run asked for a label nobody configured tells the two apart.
//!
//! Two properties, one per case: a socket bind serves an unconfigured label and
//! makes it advertise, and a HISTORY POLL ALONE does the same. Both spend a
//! river, and the advertised set is the materialized set.

mod common;

use std::time::Duration;

use common::{fast_config, http_get, spawn};
use futures_util::StreamExt;
use mogwai_protocol::InstrumentDef;
use tokio_tungstenite::tungstenite::Message;

/// A label no config in this tree mentions, wire-legal and not a preset name,
/// so it resolves to the DEFAULT bundle wearing its own label.
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

    // ABSENT first, and deliberately so: resolution is total, but nothing has
    // MATERIALIZED this label yet. A later poll or bind would - and does below.
    assert!(
        !advertises(&base, UNCONFIGURED),
        "a shape nothing has materialized must not advertise"
    );

    let (mut socket, response) = tokio_tungstenite::connect_async(venue.ws_url_for(UNCONFIGURED))
        .await
        .expect("an unconfigured label binds a socket");
    assert_eq!(response.status(), 101);

    // Drain to a DEADLINE, never asserting on the next frame: every socket is
    // attached to the live tape on upgrade, so what arrives first is whatever
    // the venue was emitting.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut labelled = false;
    while let Ok(Some(Ok(Message::Text(frame)))) =
        tokio::time::timeout_at(deadline, socket.next()).await
    {
        if frame.contains(UNCONFIGURED)
            && (frame.contains("\"Trade\"") || frame.contains("\"Quote\""))
        {
            labelled = true;
            break;
        }
    }
    assert!(labelled, "the bound river produced no market frame");

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
        &format!("/trades?symbol={UNCONFIGURED}&start=0&limit=5"),
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
    let (status, body) = http_get(&base, &format!("/trades?symbol={POLLED}&start=0&limit=5"));
    assert_eq!(status, 200, "an unconfigured poll is served: {body}");
    assert!(
        advertises(&base, POLLED),
        "a poll spends a river, and the advertised set is the materialized set"
    );
}
