// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

use std::any::Any;

use anyhow::ensure;
use mogwai_protocol::{
    HavocSpec, TransportProfile, control::Divergence, validate_client_havoc, validate_conn_havoc,
    validate_divergence, validate_market_regime,
};
use nautilus_common::factories::ClientConfig;
use nautilus_model::{
    enums::{AccountType, OmsType},
    identifiers::{AccountId, TraderId},
};
use serde::{Deserialize, Serialize};

const DEFAULT_BASE_URL: &str = "ws://127.0.0.1:8787";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MogwaiDataClientConfig {
    /// Account whose websocket session this data client is bound to.
    pub account_id: AccountId,
    /// Base URL of the running mogwai-server.
    ///
    /// Later data handlers derive the `/ws` market-data path from this value.
    /// The skeleton stores and validates the URL without opening a transport.
    pub base_url: String,
    /// Selects the transport archetype this data client presents.
    #[serde(default)]
    pub transport_profile: TransportProfile,
    /// Havoc to arm on connect. `None` is a clean adapter.
    #[serde(default)]
    pub havoc: Option<HavocSpec>,
}

impl Default for MogwaiDataClientConfig {
    fn default() -> Self {
        Self {
            account_id: AccountId::from("MOGWAI-001"),
            base_url: DEFAULT_BASE_URL.to_string(),
            transport_profile: TransportProfile::default(),
            havoc: None,
        }
    }
}

impl MogwaiDataClientConfig {
    /// Validates config invariants.
    ///
    /// # Errors
    ///
    /// Returns an error if the mogwai-server URL is empty or is not a
    /// `ws://`/`wss://` URL with a host (D.4), or if any armed havoc knob is
    /// out of range.
    pub fn validate(&self) -> anyhow::Result<()> {
        validate_base_url(&self.base_url)?;
        mogwai_protocol::AccountId::parse(self.account_id.as_ref())?;
        validate_havoc(&self.havoc, self.transport_profile)
    }

    /// Returns the ws/wss URL to hand to the transport, trimmed of
    /// surrounding whitespace. `validate_base_url` and `http_base_url` both
    /// trim; if this did not, a whitespace-padded `base_url` would pass
    /// validation and work over HTTP while `connect_async` fails on the
    /// padded ws URL silently inside the reconnect loop - the exact
    /// never-connects-with-no-diagnostic failure mode (D.4) the validator
    /// exists to rule out.
    #[must_use]
    pub fn ws_url(&self) -> String {
        format!(
            "{}?{}={}",
            self.base_url.trim(),
            mogwai_protocol::ACCOUNT_QUERY_PARAM,
            self.account_id
        )
    }

    /// Derives the HTTP base URL from the configured ws/wss `base_url`.
    ///
    /// `validate` rejects any non-ws/wss `base_url` up front (D.4), so on a
    /// validated config this always maps the scheme cleanly. For an
    /// unvalidated value with an unrecognized scheme it falls back to an
    /// `http://` prefix rather than passing the bare value through as a
    /// would-be-HTTP base (D.13); see `http_base_url`.
    #[must_use]
    pub fn http_base_url(&self) -> String {
        http_base_url(&self.base_url)
    }
}

impl ClientConfig for MogwaiDataClientConfig {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MogwaiExecClientConfig {
    pub trader_id: TraderId,
    pub account_id: AccountId,
    /// Base URL of the running mogwai-server.
    pub base_url: String,
    /// Account type reported to nautilus.
    pub account_type: AccountType,
    /// Order-management-system type the venue presents to nautilus. Defaults to
    /// `Netting` (one position per instrument). broadarrow can override this
    /// per-venue without an adapter change - `Unspecified` defers to the venue
    /// OMS, `Hedging` allows multiple positions per instrument (D.7).
    #[serde(default = "default_oms_type")]
    pub oms_type: OmsType,
    /// Selects the transport archetype this execution client presents.
    #[serde(default)]
    pub transport_profile: TransportProfile,
    /// Havoc to arm on connect. `None` is a clean adapter.
    #[serde(default)]
    pub havoc: Option<HavocSpec>,
}

/// Default OMS type for an exec client config field that is absent from the
/// deserialized payload. `OmsType::default()` is `Unspecified`, but mogwai's
/// venue has historically run as a netting OMS (the former hard-coded value,
/// D.7), so a config that omits the field keeps that behavior.
fn default_oms_type() -> OmsType {
    OmsType::Netting
}

impl Default for MogwaiExecClientConfig {
    fn default() -> Self {
        Self {
            trader_id: TraderId::from("MOGWAI-001"),
            account_id: AccountId::from("MOGWAI-001"),
            base_url: DEFAULT_BASE_URL.to_string(),
            account_type: AccountType::Cash,
            oms_type: default_oms_type(),
            transport_profile: TransportProfile::default(),
            havoc: None,
        }
    }
}

impl MogwaiExecClientConfig {
    /// Validates config invariants.
    ///
    /// # Errors
    ///
    /// Returns an error if the mogwai-server URL is empty or is not a
    /// `ws://`/`wss://` URL with a host (D.4), or if any armed havoc knob is
    /// out of range.
    pub fn validate(&self) -> anyhow::Result<()> {
        validate_base_url(&self.base_url)?;
        mogwai_protocol::AccountId::parse(self.account_id.as_ref())?;
        validate_havoc(&self.havoc, self.transport_profile)
    }

    /// Returns the ws/wss URL to hand to the transport, trimmed of
    /// surrounding whitespace. See `MogwaiDataClientConfig::ws_url` for why
    /// the trim matters (a padded URL passes validation but never connects).
    #[must_use]
    pub fn ws_url(&self) -> String {
        format!(
            "{}?{}={}",
            self.base_url.trim(),
            mogwai_protocol::ACCOUNT_QUERY_PARAM,
            self.account_id
        )
    }

    /// Derives the HTTP base URL from the configured ws/wss `base_url`.
    ///
    /// See `MogwaiDataClientConfig::http_base_url` for the scheme-mapping and
    /// unrecognized-scheme fallback contract.
    #[must_use]
    pub fn http_base_url(&self) -> String {
        http_base_url(&self.base_url)
    }
}

impl ClientConfig for MogwaiExecClientConfig {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Validates that `base_url` is a non-empty `ws://` or `wss://` URL with a
/// host. Previously `validate` only rejected empty/whitespace, so a typo like
/// `"http://x"` or `"not a url"` passed config validation and then failed
/// silently inside the reconnect loop (`connect_async` errors are swallowed),
/// manifesting as a client that never connects with no diagnostic (D.4).
/// Catching the bad scheme/host here turns that into an up-front config error.
///
/// Validation is deliberately lexical (scheme + non-empty authority) rather
/// than a full URL parse: the workspace pulls in no URL crate, `connect_async`
/// is the real authority on the rest of the syntax, and this only needs to rule
/// out the two failure modes the finding names.
fn validate_base_url(base_url: &str) -> anyhow::Result<()> {
    let trimmed = base_url.trim();
    ensure!(!trimmed.is_empty(), "base_url cannot be empty");
    let authority = trimmed
        .strip_prefix("ws://")
        .or_else(|| trimmed.strip_prefix("wss://"))
        .ok_or_else(|| {
            anyhow::anyhow!("base_url must be a ws:// or wss:// URL, got {base_url:?}")
        })?;
    // The host (with optional port) is everything before the first path /query
    // /fragment delimiter; reject an empty authority such as `ws:///path`.
    let host = authority.split(['/', '?', '#']).next().unwrap_or_default();
    ensure!(
        !host.is_empty(),
        "base_url must include a host, got {base_url:?}"
    );
    Ok(())
}

fn http_base_url(base_url: &str) -> String {
    let trimmed = base_url.trim();
    if let Some(rest) = trimmed.strip_prefix("ws://") {
        format!("http://{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("wss://") {
        format!("https://{rest}")
    } else if let Some(rest) = trimmed
        .strip_prefix("http://")
        .or_else(|| trimmed.strip_prefix("https://"))
    {
        // Already an HTTP(S) authority; normalize back to itself so an
        // accidentally http-typed base still yields a real HTTP base instead
        // of being mangled. (Such a value never passes `validate_base_url`,
        // which requires ws/wss - this only matters for a direct caller.)
        if trimmed.starts_with("https://") {
            format!("https://{rest}")
        } else {
            format!("http://{rest}")
        }
    } else {
        // No recognized scheme. The old code returned the bare/relative value
        // unchanged, yielding an "HTTP base" that is not HTTP (D.13). Prefix
        // `http://` so the result is at least a well-formed HTTP URL rather
        // than a relative path silently used as a base. `validate_base_url`
        // rejects this case at config time, so the live path never reaches it.
        format!("http://{trimmed}")
    }
}

/// Runs the full havoc validation both adapter configs share: the client-side
/// probabilities, the connection-lifecycle knobs, the optional market regime,
/// and every armed server `Divergence`. Single-sourcing this here keeps the
/// two configs from drifting and means an out-of-range knob (an unbounded
/// `PartialFillNext.fraction`, a degenerate regime, a zeroed rate limit) is
/// rejected at config time rather than detonating later on the live path.
fn validate_havoc(
    havoc: &Option<HavocSpec>,
    transport_profile: TransportProfile,
) -> anyhow::Result<()> {
    if let Some(havoc) = havoc {
        validate_client_havoc(&havoc.client).map_err(anyhow::Error::msg)?;
        validate_conn_havoc(&havoc.conn).map_err(anyhow::Error::msg)?;
        if let Some(regime) = &havoc.data {
            validate_market_regime(regime).map_err(anyhow::Error::msg)?;
        }
        for divergence in &havoc.server {
            validate_divergence(divergence).map_err(anyhow::Error::msg)?;
            validate_window_deliverable(divergence, transport_profile)?;
        }
    }
    Ok(())
}

/// Refuses a server temporal window the chosen transport carrier provably
/// cannot deliver. The windows live only in the server's `/ws` outbound
/// writer, so a leg carried over HTTP request/response never passes through
/// them: arming such a window constructs a client that runs a clean path
/// while the operator believes havoc is live - a blackout test that passes
/// green against a path that was never dark. Every other impossible spec is
/// already refused at create, and both contradicting fields sit in this one
/// config, so the contradiction is refused at the same boundary:
///
/// - `DelayAcks` paces execution frames on the WS socket; under
///   `HttpOrders`/`HttpPolling` order events return synchronously in the
///   `POST /orders` response and there is no WS exec leg to delay.
/// - `GoDark` is documented as a TOTAL venue blackout; under either HTTP
///   orders profile the order path rides HTTP and stays live through the
///   window, so the blackout can never be total (under `HttpOrders` a
///   data-only stall is deliverable - arm `StallData` instead).
/// - `StallData` gates WS market-data frames; under `HttpPolling` data is
///   fetched over `GET /trades` and never passes the writer. It stays legal
///   under `HttpOrders`, whose market data still streams over WS.
///
/// `ClearDivergences` and the engine-side single-shots are carrier-agnostic
/// and pass unconditionally.
fn validate_window_deliverable(
    divergence: &Divergence,
    transport_profile: TransportProfile,
) -> anyhow::Result<()> {
    match divergence {
        Divergence::DelayAcks { .. } if transport_profile.orders_over_http() => {
            anyhow::bail!(
                "DelayAcks delays execution frames on the /ws socket, but \
                 transport_profile {transport_profile:?} carries order entry over HTTP \
                 request/response - the delay can never fire; use WsStreaming"
            )
        }
        Divergence::GoDark { .. } if transport_profile.orders_over_http() => {
            anyhow::bail!(
                "GoDark is a total venue blackout on the /ws writer, but \
                 transport_profile {transport_profile:?} carries order entry over HTTP and \
                 stays live through the window; use WsStreaming (or StallData under \
                 HttpOrders for a data-only stall)"
            )
        }
        Divergence::StallData { .. } if transport_profile.data_by_polling() => {
            anyhow::bail!(
                "StallData gates market-data frames on the /ws socket, but \
                 transport_profile {transport_profile:?} polls market data over GET /trades - \
                 the stall can never fire; use WsStreaming or HttpOrders"
            )
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_base_url_uses_server_port() {
        let cfg = MogwaiDataClientConfig::default();

        assert_eq!(cfg.ws_url(), "ws://127.0.0.1:8787?account=MOGWAI-001");
        assert_eq!(cfg.http_base_url(), "http://127.0.0.1:8787");
    }

    #[test]
    fn http_base_url_normalizes_secure_ws_scheme() {
        let cfg = MogwaiDataClientConfig {
            base_url: "wss://example.test:9443".into(),
            ..MogwaiDataClientConfig::default()
        };

        assert_eq!(cfg.ws_url(), "wss://example.test:9443?account=MOGWAI-001");
        assert_eq!(cfg.http_base_url(), "https://example.test:9443");
    }

    #[test]
    fn a_config_id_illegal_to_the_venue_fails_client_construction() {
        // The two charsets do not agree: nautilus `AccountId::new_checked` wants
        // only a non-empty ASCII string with a `-` and non-empty parts either
        // side, so it accepts spaces and slashes the venue refuses. Left
        // unchecked, a perfectly legal nautilus config yields a client that
        // 400s on every request, first observed under live trading.
        for illegal in ["WYRD 042-A", "WYRD-042/BTCUSDT"] {
            let data = MogwaiDataClientConfig {
                account_id: AccountId::from(illegal),
                ..MogwaiDataClientConfig::default()
            };
            let exec = MogwaiExecClientConfig {
                account_id: AccountId::from(illegal),
                ..MogwaiExecClientConfig::default()
            };
            let err = data
                .validate()
                .expect_err("a venue-illegal id must fail the data client build")
                .to_string();
            assert!(err.contains("illegal character"), "{err}");
            assert!(exec.validate().is_err(), "{illegal}");
        }
        // The deployment shape stays legal to both ends.
        assert!(
            MogwaiExecClientConfig {
                account_id: AccountId::from("WYRD-042:BTCUSDT"),
                ..MogwaiExecClientConfig::default()
            }
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn websocket_upgrade_carries_the_account_query_param() {
        let cfg = MogwaiExecClientConfig {
            account_id: AccountId::from("WYRD-042:BTCUSDT"),
            base_url: "ws://example.test:8787  ".into(),
            ..MogwaiExecClientConfig::default()
        };
        assert_eq!(
            cfg.ws_url(),
            "ws://example.test:8787?account=WYRD-042:BTCUSDT"
        );
    }

    #[test]
    fn validate_rejects_non_ws_scheme() {
        // D.4: a typo'd scheme used to pass validation and then fail silently
        // inside the reconnect loop. Reject it up front.
        let http = MogwaiDataClientConfig {
            base_url: "http://example.test".into(),
            ..MogwaiDataClientConfig::default()
        };
        let garbage = MogwaiDataClientConfig {
            base_url: "not a url".into(),
            ..MogwaiDataClientConfig::default()
        };

        assert!(http.validate().is_err());
        assert!(garbage.validate().is_err());
    }

    #[test]
    fn validate_rejects_hostless_ws_url() {
        let cfg = MogwaiExecClientConfig {
            base_url: "ws:///just/a/path".into(),
            ..MogwaiExecClientConfig::default()
        };

        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_accepts_ws_and_wss() {
        let ws = MogwaiDataClientConfig {
            base_url: "ws://example.test:8787".into(),
            ..MogwaiDataClientConfig::default()
        };
        let wss = MogwaiExecClientConfig {
            base_url: "wss://example.test:9443".into(),
            ..MogwaiExecClientConfig::default()
        };

        assert!(ws.validate().is_ok());
        assert!(wss.validate().is_ok());
    }

    #[test]
    fn ws_url_trims_whitespace_padding() {
        // A whitespace-padded base_url passes validation (which trims) and
        // derives a clean HTTP base (which trims), so ws_url must trim too or
        // the padded value reaches connect_async and fails silently forever
        // inside the reconnect loop.
        let data = MogwaiDataClientConfig {
            base_url: "  ws://example.test:8787  ".into(),
            ..MogwaiDataClientConfig::default()
        };
        let exec = MogwaiExecClientConfig {
            base_url: "\twss://example.test:9443\n".into(),
            ..MogwaiExecClientConfig::default()
        };

        assert!(data.validate().is_ok());
        assert_eq!(data.ws_url(), "ws://example.test:8787?account=MOGWAI-001");
        assert_eq!(data.http_base_url(), "http://example.test:8787");

        assert!(exec.validate().is_ok());
        assert_eq!(exec.ws_url(), "wss://example.test:9443?account=MOGWAI-001");
        assert_eq!(exec.http_base_url(), "https://example.test:9443");
    }

    #[test]
    fn validate_refuses_undeliverable_temporal_windows() {
        use mogwai_protocol::control::Divergence;

        let config = |divergence: Divergence, profile: TransportProfile| MogwaiExecClientConfig {
            transport_profile: profile,
            havoc: Some(HavocSpec {
                server: vec![divergence],
                ..HavocSpec::default()
            }),
            ..MogwaiExecClientConfig::default()
        };

        // The full deliverability matrix. A window a carrier cannot pass is a
        // create-time refusal, exactly like every other impossible spec.
        let cases = [
            (
                Divergence::DelayAcks { ms: 100 },
                TransportProfile::WsStreaming,
                true,
            ),
            (
                Divergence::DelayAcks { ms: 100 },
                TransportProfile::HttpOrders,
                false,
            ),
            (
                Divergence::DelayAcks { ms: 100 },
                TransportProfile::HttpPolling,
                false,
            ),
            (
                Divergence::GoDark { ms: 100 },
                TransportProfile::WsStreaming,
                true,
            ),
            (
                Divergence::GoDark { ms: 100 },
                TransportProfile::HttpOrders,
                false,
            ),
            (
                Divergence::GoDark { ms: 100 },
                TransportProfile::HttpPolling,
                false,
            ),
            (
                Divergence::StallData { ms: 100 },
                TransportProfile::WsStreaming,
                true,
            ),
            // HttpOrders still streams market data over WS, so a data-only
            // stall is deliverable there.
            (
                Divergence::StallData { ms: 100 },
                TransportProfile::HttpOrders,
                true,
            ),
            (
                Divergence::StallData { ms: 100 },
                TransportProfile::HttpPolling,
                false,
            ),
            // Carrier-agnostic controls pass under every profile.
            (
                Divergence::ClearDivergences,
                TransportProfile::HttpPolling,
                true,
            ),
            (
                Divergence::RejectNextSubmit {
                    reason: "nope".into(),
                },
                TransportProfile::HttpPolling,
                true,
            ),
        ];
        for (divergence, profile, ok) in cases {
            let result = config(divergence.clone(), profile).validate();
            assert_eq!(
                result.is_ok(),
                ok,
                "{divergence:?} under {profile:?}: {result:?}"
            );
        }

        // The data config shares the same validator, so the same contradiction
        // is refused there too.
        let data = MogwaiDataClientConfig {
            transport_profile: TransportProfile::HttpPolling,
            havoc: Some(HavocSpec {
                server: vec![Divergence::GoDark { ms: 100 }],
                ..HavocSpec::default()
            }),
            ..MogwaiDataClientConfig::default()
        };
        assert!(data.validate().is_err());
    }

    #[test]
    fn http_base_url_never_emits_a_non_http_base() {
        // D.13: an unrecognized scheme used to pass through unchanged, yielding
        // an "HTTP base" that is not HTTP. It now always carries an http(s)
        // scheme. (Such values are rejected by `validate`; this guards a direct
        // caller.)
        assert_eq!(
            super::http_base_url("relative/path"),
            "http://relative/path"
        );
        assert_eq!(
            super::http_base_url("http://already.http"),
            "http://already.http"
        );
        assert_eq!(
            super::http_base_url("https://already.https"),
            "https://already.https"
        );
    }
}
