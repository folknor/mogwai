// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

use std::any::Any;

use anyhow::ensure;
use mogwai_protocol::{HavocSpec, validate_client_havoc, validate_conn_havoc, validate_divergence};
use nautilus_common::factories::ClientConfig;
use nautilus_model::{
    enums::{AccountType, OmsType},
    identifiers::{AccountId, TraderId},
};
use serde::{Deserialize, Serialize};

/// Default local Nautilus account label. Mogwai has one ledger per run and
/// carries no account identity on the wire.
pub const DEFAULT_ACCOUNT_ID: &str = "MOGWAI-001";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MogwaiDataClientConfig {
    /// Local Nautilus account label attached to client metadata.
    pub account_id: AccountId,
    /// Base URL of the running mogwai-server.
    ///
    /// Later data handlers derive the `/ws` market-data path from this value.
    /// The skeleton stores and validates the URL without opening a transport.
    pub base_url: String,
    /// Havoc to arm on connect. `None` is a clean adapter.
    #[serde(default)]
    pub havoc: Option<HavocSpec>,
    /// The run this client belongs to, checked on every connect.
    ///
    /// `None` keeps the historical behaviour: dial the address and trust
    /// whatever answers. Set - which [`MogwaiDataClientConfig::for_run`] does
    /// from the readiness record - the client verifies the venue's reported
    /// `run_seed` before using the connection, and refuses terminally if it
    /// differs. See `verify_run_identity`.
    #[serde(default)]
    pub expected_run_seed: Option<u64>,
}

impl Default for MogwaiDataClientConfig {
    fn default() -> Self {
        Self {
            account_id: AccountId::from(DEFAULT_ACCOUNT_ID),
            base_url: String::new(),
            havoc: None,
            expected_run_seed: None,
        }
    }
}

impl MogwaiDataClientConfig {
    /// Build a config for the address a launched venue reported.
    ///
    /// The endpoint is not choosable and not guessable, so it always arrives as
    /// a `SocketAddr` out of a `ReadyRecord`; taking it directly removes the
    /// `ws://{addr}` formatting from every call site, where getting the scheme
    /// wrong fails as a connect timeout inside the reconnect loop rather than as
    /// anything that names the cause.
    ///
    #[must_use]
    pub fn for_addr(addr: std::net::SocketAddr, account_id: AccountId) -> Self {
        Self {
            account_id,
            base_url: format!("ws://{addr}"),
            havoc: None,
            expected_run_seed: None,
        }
    }

    /// Build a config bound to the RUN a readiness record describes, not merely
    /// to the address it landed on.
    ///
    /// Prefer this to [`Self::for_addr`] whenever the record is in hand, which
    /// it always is when the venue was launched rather than found. An address
    /// identifies nothing over time: the port is ephemeral, and this venue frees
    /// it before it exits, so a client that only knows where to dial cannot tell
    /// its own run from whatever answers there next.
    #[must_use]
    pub fn for_run(record: &mogwai_protocol::ReadyRecord, account_id: AccountId) -> Self {
        Self {
            expected_run_seed: Some(record.run_seed),
            ..Self::for_addr(record.addr, account_id)
        }
    }

    /// Arm havoc on this config, for the builder-ish call sites that want one
    /// expression.
    #[must_use]
    pub fn with_havoc(mut self, havoc: Option<HavocSpec>) -> Self {
        self.havoc = havoc;
        self
    }

    /// Validates config invariants.
    ///
    /// # Errors
    ///
    /// Returns an error if the mogwai-server URL is empty or is not a
    /// `ws://`/`wss://` URL with a host (D.4), or if any armed havoc knob is
    /// out of range.
    pub fn validate(&self) -> anyhow::Result<()> {
        validate_base_url(&self.base_url)?;
        validate_account_id(&self.account_id)?;
        validate_havoc(&self.havoc)
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
        format!("{}/ws", self.base_url.trim().trim_end_matches('/'))
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
    /// `Netting` (one position per instrument). The host can override this
    /// per-venue without an adapter change - `Unspecified` defers to the venue
    /// OMS, `Hedging` allows multiple positions per instrument (D.7).
    #[serde(default = "default_oms_type")]
    pub oms_type: OmsType,
    /// Havoc to arm on connect. `None` is a clean adapter.
    #[serde(default)]
    pub havoc: Option<HavocSpec>,
    /// The run this client belongs to. See
    /// [`MogwaiDataClientConfig::expected_run_seed`]; both legs should carry the
    /// same one, for the same reason they carry the same account.
    #[serde(default)]
    pub expected_run_seed: Option<u64>,
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
            account_id: AccountId::from(DEFAULT_ACCOUNT_ID),
            base_url: String::new(),
            account_type: AccountType::Cash,
            oms_type: default_oms_type(),
            havoc: None,
            expected_run_seed: None,
        }
    }
}

impl MogwaiExecClientConfig {
    /// Build a config for the address a launched venue reported.
    ///
    /// See [`MogwaiDataClientConfig::for_addr`]. Prefer [`Self::for_run`] when
    /// the readiness record is in hand.
    ///
    /// Leaves `account_type` at `Cash`. A futures run wants
    /// `AccountType::Margin` - the venue posts and reports margin either way,
    /// but a nautilus `CashAccount` has nowhere to keep the rows it receives, so
    /// they are dropped client-side. See `docs/oms-types.md`.
    #[must_use]
    pub fn for_addr(addr: std::net::SocketAddr, account_id: AccountId) -> Self {
        Self {
            account_id,
            base_url: format!("ws://{addr}"),
            ..Self::default()
        }
    }

    /// Build a config bound to the RUN a readiness record describes. See
    /// [`MogwaiDataClientConfig::for_run`].
    #[must_use]
    pub fn for_run(record: &mogwai_protocol::ReadyRecord, account_id: AccountId) -> Self {
        Self {
            expected_run_seed: Some(record.run_seed),
            ..Self::for_addr(record.addr, account_id)
        }
    }

    /// Arm havoc on this config.
    #[must_use]
    pub fn with_havoc(mut self, havoc: Option<HavocSpec>) -> Self {
        self.havoc = havoc;
        self
    }

    /// Set the trader id. [`Self::for_addr`] leaves it at this crate's default,
    /// which is a mogwai-flavoured placeholder rather than the host's identity,
    /// so a host with its own trader id sets it here.
    #[must_use]
    pub fn with_trader_id(mut self, trader_id: TraderId) -> Self {
        self.trader_id = trader_id;
        self
    }

    /// Set the OMS type this client presents to nautilus.
    #[must_use]
    pub fn with_oms_type(mut self, oms_type: OmsType) -> Self {
        self.oms_type = oms_type;
        self
    }

    /// Set the account type. `Margin` is what a futures run wants.
    #[must_use]
    pub fn with_account_type(mut self, account_type: AccountType) -> Self {
        self.account_type = account_type;
        self
    }

    /// Validates config invariants.
    ///
    /// # Errors
    ///
    /// Returns an error if the mogwai-server URL is empty or is not a
    /// `ws://`/`wss://` URL with a host (D.4), or if any armed havoc knob is
    /// out of range.
    pub fn validate(&self) -> anyhow::Result<()> {
        validate_base_url(&self.base_url)?;
        validate_account_id(&self.account_id)?;
        validate_havoc(&self.havoc)
    }

    /// Returns the ws/wss URL to hand to the transport, trimmed of
    /// surrounding whitespace. See `MogwaiDataClientConfig::ws_url` for why
    /// the trim matters (a padded URL passes validation but never connects).
    #[must_use]
    pub fn ws_url(&self) -> String {
        format!("{}/ws", self.base_url.trim().trim_end_matches('/'))
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

/// Validates the local Nautilus account label against mogwai's wire-safe
/// charset. The label is not sent to the one-ledger venue.
fn validate_account_id(account_id: &AccountId) -> anyhow::Result<()> {
    mogwai_protocol::AccountId::parse(account_id.as_ref())?;
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
fn validate_havoc(havoc: &Option<HavocSpec>) -> anyhow::Result<()> {
    if let Some(havoc) = havoc {
        validate_client_havoc(&havoc.client).map_err(anyhow::Error::msg)?;
        validate_conn_havoc(&havoc.conn).map_err(anyhow::Error::msg)?;
        for divergence in &havoc.server {
            validate_divergence(divergence).map_err(anyhow::Error::msg)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_account_label_is_valid_because_it_is_local_only() {
        let data = MogwaiDataClientConfig {
            base_url: "ws://127.0.0.1:1".into(),
            ..MogwaiDataClientConfig::default()
        };
        data.validate().unwrap();

        let exec = MogwaiExecClientConfig {
            base_url: "ws://127.0.0.1:1".into(),
            ..MogwaiExecClientConfig::default()
        };
        exec.validate().unwrap();
    }
}
