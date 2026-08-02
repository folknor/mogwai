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

/// Placeholder both configs default their `account_id` to, and which `validate`
/// refuses.
///
/// `account_id` is the one field of these configs that has no defensible
/// default: it names WHICH server-side account slot the socket binds to, and
/// the data and execution legs of one venue session must bind the SAME slot or
/// the server-owned divergence windows (`StallData`, `GoDark`, the delay
/// atomics), which live on that slot and are armed only from the execution leg,
/// silently miss the data feed entirely. A default that looked like a real
/// account (`MOGWAI-001`) made an omitted `account_id` indistinguishable from a
/// deliberate one, so a consumer that forgot the field got a working data
/// socket on the wrong account and no diagnostic anywhere. Defaulting to a value
/// that is syntactically legal - so `Default`, `#[serde(default)]` and every
/// partial-config deserialization keep working - but semantically refused turns
/// that omission into a create-time error naming the field.
pub const UNSET_ACCOUNT_ID: &str = "MOGWAI-UNSET";

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
    /// Havoc to arm on connect. `None` is a clean adapter.
    #[serde(default)]
    pub havoc: Option<HavocSpec>,
}

impl Default for MogwaiDataClientConfig {
    fn default() -> Self {
        Self {
            account_id: AccountId::from(UNSET_ACCOUNT_ID),
            base_url: String::new(),
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
        // The PATH goes on before the query string, which is the whole reason
        // this builds the socket URL rather than returning a base for a caller
        // to append to. Appending `/ws` to a value already ending in
        // `?account=...` yields `ws://host?account=X/ws`, whose account is the
        // literal `X/ws` and whose path is `/` - a URL the venue rejects, and
        // which fails as a connect timeout inside the reconnect loop rather
        // than as anything that names the cause.
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
    /// `Netting` (one position per instrument). broadarrow can override this
    /// per-venue without an adapter change - `Unspecified` defers to the venue
    /// OMS, `Hedging` allows multiple positions per instrument (D.7).
    #[serde(default = "default_oms_type")]
    pub oms_type: OmsType,
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
            account_id: AccountId::from(UNSET_ACCOUNT_ID),
            base_url: String::new(),
            account_type: AccountType::Cash,
            oms_type: default_oms_type(),
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
        validate_account_id(&self.account_id)?;
        validate_havoc(&self.havoc)
    }

    /// Returns the ws/wss URL to hand to the transport, trimmed of
    /// surrounding whitespace. See `MogwaiDataClientConfig::ws_url` for why
    /// the trim matters (a padded URL passes validation but never connects).
    #[must_use]
    pub fn ws_url(&self) -> String {
        // The PATH goes on before the query string, which is the whole reason
        // this builds the socket URL rather than returning a base for a caller
        // to append to. Appending `/ws` to a value already ending in
        // `?account=...` yields `ws://host?account=X/ws`, whose account is the
        // literal `X/ws` and whose path is `/` - a URL the venue rejects, and
        // which fails as a connect timeout inside the reconnect loop rather
        // than as anything that names the cause.
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

/// Validates the account this client's socket binds to: it must be legal to the
/// venue's own charset, and it must not still be the `UNSET_ACCOUNT_ID`
/// placeholder.
///
/// The placeholder check is the whole point of the placeholder. The data and
/// execution legs of one venue session must bind the same server account slot -
/// the divergence windows live on the slot and only the execution leg arms them,
/// so a data leg on a different slot streams straight through every armed
/// `StallData` and `GoDark`. There is nothing in a single config that can detect
/// that mismatch, and nothing on the wire that reports it: the venue happily
/// auto-creates whatever account a socket names. Refusing the value a config
/// carries when nobody set it is the one place the omission is knowable, so it
/// is refused here, before any socket is opened.
fn validate_account_id(account_id: &AccountId) -> anyhow::Result<()> {
    ensure!(
        account_id.as_ref() != UNSET_ACCOUNT_ID,
        "account_id is still the {UNSET_ACCOUNT_ID} placeholder - set it to the \
         account this client binds to on the venue. The data and execution \
         clients of one venue session must be given the SAME account_id, or \
         server-armed divergence windows will miss the market-data feed"
    );
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
