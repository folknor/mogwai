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

/// Default account label, used both as the Nautilus account identity and as the
/// ledger this client names on the venue's `/ws?account=`. It matches the
/// venue's own default account id, so an ephemeral one-run venue that was never
/// told about accounts serves exactly the ledger it always did.
pub const DEFAULT_ACCOUNT_ID: &str = "MOGWAI-001";

/// This PROCESS's client identity, presented on `/ws?session=` by every client
/// this adapter builds.
///
/// The venue seats one client per ledger and evicts a second one, because from
/// its side a socket presenting an already-seated account is indistinguishable
/// from that client reconnecting. A nautilus host holds TWO sockets on one
/// ledger by construction - the data client and the execution client - so
/// without a shared identity the second dial would evict the first and the host
/// would disconnect itself before it ever traded.
///
/// PER PROCESS is the right granularity, and it is the whole design. One worker
/// process is one client: its two legs share this value, and so does every
/// redial either of them makes, so neither can evict the other. A RESTARTED
/// worker gets a fresh one, which is exactly when eviction is wanted - the
/// venue's frozen ledger is being reclaimed by a genuinely new process, and the
/// stale sockets of the dead one must go.
///
/// It is derived from the pid and a wall-clock instant rather than randomly, so
/// it is stable for the life of the process without any state to keep, and two
/// processes cannot collide: a reused pid arrives with a later instant. The
/// instant is the one this `OnceLock` initializes at - the first client this
/// process builds - not the process's start, which is a distinction with no
/// consequence for either property and is stated because the two are easy to
/// conflate and the argument should rest on what the code does.
fn process_session_id() -> &'static str {
    static SESSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SESSION.get_or_init(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        format!("mogwai-{pid}-{nanos}", pid = std::process::id())
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MogwaiDataClientConfig {
    /// The account this client trades under, both as the Nautilus label
    /// attached to client metadata and as the venue ledger named on
    /// `/ws?account=`. The two are deliberately one field: a host whose feed
    /// and execution named different ledgers would arm divergences on one and
    /// read frames from the other.
    pub account_id: AccountId,
    /// Base URL of the running mogwai-server.
    ///
    /// Later data handlers derive the `/ws` market-data path from this value.
    /// The skeleton stores and validates the URL without opening a transport.
    pub base_url: String,
    /// River named on the websocket upgrade. `None` takes the server's boot
    /// symbol for compatibility with clients predating the carrier.
    #[serde(default)]
    pub symbol: Option<String>,
    /// Havoc to arm on connect. `None` is a clean adapter.
    #[serde(default)]
    pub havoc: Option<HavocSpec>,
    /// The CLIENT identity presented on `/ws?session=`, so this process's
    /// several sockets on one ledger are not read as several clients. Defaults
    /// to [`process_session_id`] and should stay there; see it for why. `None`
    /// sends no session and takes the venue's always-evict reading, which is
    /// what a host wants only if it holds exactly one socket per ledger.
    #[serde(default = "default_session")]
    pub session: Option<String>,
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
            symbol: None,
            havoc: None,
            session: default_session(),
            expected_run_seed: None,
        }
    }
}

/// Serde default for both configs' `session`: this process's identity.
fn default_session() -> Option<String> {
    Some(process_session_id().to_owned())
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
            ..Self::default()
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
    ///
    /// It names no river. A venue reports no symbol (`ReadyRecord` 6) because it
    /// can serve many, so `symbol` stays `None` and the socket lands on the
    /// venue's boot river; a host that wants a specific one says so with
    /// [`Self::with_symbol`].
    #[must_use]
    pub fn for_run(record: &mogwai_protocol::ReadyRecord, account_id: AccountId) -> Self {
        Self {
            expected_run_seed: Some(record.run_seed),
            ..Self::for_addr(record.addr, account_id)
        }
    }

    /// Name the river this client's socket binds, matching `/ws?symbol=`.
    /// Without one the socket takes the venue's boot river. This is the host's
    /// choice and is not derivable from a venue readiness record.
    #[must_use]
    pub fn with_symbol(mut self, symbol: impl Into<String>) -> Self {
        self.symbol = Some(symbol.into());
        self
    }

    /// Arm havoc on this config, for the builder-ish call sites that want one
    /// expression.
    #[must_use]
    pub fn with_havoc(mut self, havoc: Option<HavocSpec>) -> Self {
        self.havoc = havoc;
        self
    }

    /// Override the client identity presented on `/ws?session=`. The default is
    /// this process's, which is what a host wants; set it only to make two
    /// clients in ONE process deliberately distinct clients of one ledger -
    /// which is a request to have them evict each other - or to `None` to take
    /// the venue's always-evict reading.
    #[must_use]
    pub fn with_session(mut self, session: Option<String>) -> Self {
        self.session = session;
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
        validate_symbol(self.symbol.as_deref())?;
        validate_session(self.session.as_deref())?;
        validate_havoc(&self.havoc)
    }

    /// Returns the ws/wss URL to hand to the transport, trimmed of
    /// surrounding whitespace. `validate_base_url` and `http_base_url` both
    /// trim; if this did not, a whitespace-padded `base_url` would pass
    /// validation and work over HTTP while `connect_async` fails on the
    /// padded ws URL silently inside the reconnect loop - the exact
    /// never-connects-with-no-diagnostic failure mode (D.4) the validator
    /// exists to rule out.
    ///
    /// The symbol, account and session are appended raw and need no percent
    /// encoding: `validate` refuses every byte outside their wire alphabets
    /// first. An absent symbol takes the server's boot river.
    #[must_use]
    pub fn ws_url(&self) -> String {
        ws_url(
            &self.base_url,
            self.symbol.as_deref(),
            &self.account_id,
            self.session.as_deref(),
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
    /// River named on the websocket upgrade. `None` takes the server default.
    #[serde(default)]
    pub symbol: Option<String>,
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
    /// The CLIENT identity presented on `/ws?session=`. See
    /// [`MogwaiDataClientConfig::session`]; both legs must carry the SAME one,
    /// which is what stops them evicting each other off their shared ledger.
    #[serde(default = "default_session")]
    pub session: Option<String>,
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
            symbol: None,
            account_type: AccountType::Cash,
            oms_type: default_oms_type(),
            havoc: None,
            session: default_session(),
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

    /// Name the river this client's socket binds, matching `/ws?symbol=`.
    /// Without one the socket takes the venue's boot river. This is the host's
    /// choice and is not derivable from a venue readiness record.
    #[must_use]
    pub fn with_symbol(mut self, symbol: impl Into<String>) -> Self {
        self.symbol = Some(symbol.into());
        self
    }

    /// Arm havoc on this config.
    #[must_use]
    pub fn with_havoc(mut self, havoc: Option<HavocSpec>) -> Self {
        self.havoc = havoc;
        self
    }

    /// Override the client identity presented on `/ws?session=`. See
    /// [`MogwaiDataClientConfig::with_session`]; the two legs of one host must
    /// agree, so overriding one means overriding both.
    #[must_use]
    pub fn with_session(mut self, session: Option<String>) -> Self {
        self.session = session;
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
        validate_symbol(self.symbol.as_deref())?;
        validate_session(self.session.as_deref())?;
        validate_havoc(&self.havoc)
    }

    /// Returns the ws/wss URL to hand to the transport, trimmed of
    /// surrounding whitespace. See `MogwaiDataClientConfig::ws_url` for why
    /// the trim matters (a padded URL passes validation but never connects),
    /// and for why the query values are appended without encoding.
    #[must_use]
    pub fn ws_url(&self) -> String {
        ws_url(
            &self.base_url,
            self.symbol.as_deref(),
            &self.account_id,
            self.session.as_deref(),
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

/// Build the `/ws` upgrade URL both clients dial.
///
/// THE ACCOUNT IS ALWAYS NAMED, which is the whole point of it being here. The
/// venue resolves accounts totally - a socket naming none is served under the
/// venue's default - so an unnamed socket silently traded whatever ledger the
/// venue happened to call default, whatever the host's config said. That was
/// invisible on the wire and made every attached worker share one book. Naming
/// it makes the host's configured id the ledger it actually gets, and costs an
/// ephemeral single-client venue nothing because the default id on both sides
/// is the same string.
///
/// The session rides with it for the reason [`process_session_id`] states: the
/// two clients of one host name one account, and the venue would otherwise read
/// the second dial as a stranger claiming the ledger.
fn ws_url(
    base_url: &str,
    symbol: Option<&str>,
    account_id: &AccountId,
    session: Option<&str>,
) -> String {
    let base = base_url.trim().trim_end_matches('/');
    let mut url = format!("{base}/ws?account={account}", account = account_id.as_ref());
    if let Some(symbol) = symbol {
        url.push_str(&format!("&symbol={symbol}"));
    }
    if let Some(session) = session {
        url.push_str(&format!("&session={session}"));
    }
    url
}

/// Refuse a session id the `/ws` URL cannot carry, by the rule the server
/// judges the decoded value with, so the two ends cannot drift.
fn validate_session(session: Option<&str>) -> anyhow::Result<()> {
    if let Some(session) = session
        && let Err(reason) = mogwai_protocol::validate_session_id(session)
    {
        anyhow::bail!(
            "session {session:?} cannot be carried directly in a websocket URL: {reason}"
        );
    }
    Ok(())
}

/// Refuse a symbol the `/ws` URL cannot carry.
///
/// The URL is built by CONCATENATION, so an illegal symbol must fail at config
/// validation rather than as an unreadable `400` from inside the reconnect
/// loop. The rule is `mogwai_protocol::validate_wire_symbol`, the one the
/// server judges the decoded value by, so the two ends cannot drift.
fn validate_symbol(symbol: Option<&str>) -> anyhow::Result<()> {
    if let Some(symbol) = symbol
        && let Err(reason) = mogwai_protocol::validate_wire_symbol(symbol)
    {
        anyhow::bail!("symbol {symbol:?} cannot be carried directly in a websocket URL: {reason}");
    }
    Ok(())
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

/// Validates the account label against mogwai's wire-safe charset. It IS sent -
/// it names the venue ledger on `/ws?account=` - so this is a URL-carriage
/// check as much as a Nautilus one, and it is the venue's own `AccountId::parse`
/// so the two ends cannot judge it differently.
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

    fn ready_record() -> mogwai_protocol::ReadyRecord {
        mogwai_protocol::ReadyRecord {
            version: mogwai_protocol::ReadyRecord::VERSION,
            addr: "127.0.0.1:1234".parse().expect("address"),
            pid: 1,
            run_seed: 7,
            data_origin_ns: 0,
            run_start_ns: 0,
            run_duration_ns: Some(1_000_000_000),
            warmup_ns: 0,
            reset_account_on_reconnect: false,
            account_ttl_ms: 0,
            version_string: "test".into(),
        }
    }

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

    #[test]
    fn ws_url_appends_a_configured_symbol() {
        let config = MogwaiDataClientConfig {
            base_url: "ws://127.0.0.1:1".into(),
            symbol: Some("MNQ".into()),
            session: None,
            ..MogwaiDataClientConfig::default()
        };
        assert_eq!(
            config.ws_url(),
            "ws://127.0.0.1:1/ws?account=MOGWAI-001&symbol=MNQ"
        );
    }

    #[test]
    fn ws_url_omits_an_absent_symbol() {
        let config = MogwaiExecClientConfig {
            base_url: "ws://127.0.0.1:1".into(),
            session: None,
            ..MogwaiExecClientConfig::default()
        };
        assert_eq!(config.ws_url(), "ws://127.0.0.1:1/ws?account=MOGWAI-001");
    }

    /// The ledger is NAMED, and it is the configured one rather than whatever
    /// the venue calls default. This is the whole of the consumer-visible fix:
    /// without it every attached worker seated one shared book.
    #[test]
    fn ws_url_names_the_configured_account_on_both_legs() {
        let data = MogwaiDataClientConfig {
            base_url: "ws://127.0.0.1:1".into(),
            account_id: AccountId::from("CLAUDETTE-07"),
            session: None,
            ..MogwaiDataClientConfig::default()
        };
        let exec = MogwaiExecClientConfig {
            base_url: "ws://127.0.0.1:1".into(),
            account_id: AccountId::from("CLAUDETTE-07"),
            session: None,
            ..MogwaiExecClientConfig::default()
        };
        assert_eq!(data.ws_url(), "ws://127.0.0.1:1/ws?account=CLAUDETTE-07");
        assert_eq!(exec.ws_url(), "ws://127.0.0.1:1/ws?account=CLAUDETTE-07");
    }

    /// The two legs of one host present ONE client identity, which is what
    /// stops the venue reading the second dial as a stranger claiming the
    /// ledger and evicting the first.
    #[test]
    fn both_legs_default_to_the_same_process_session() {
        let data = MogwaiDataClientConfig::default();
        let exec = MogwaiExecClientConfig::default();
        assert_eq!(data.session, exec.session);
        assert!(
            data.session.is_some(),
            "a host that configures nothing still gets an identity"
        );
        let config = MogwaiDataClientConfig {
            base_url: "ws://127.0.0.1:1".into(),
            ..MogwaiDataClientConfig::default()
        };
        let session = config.session.clone().expect("a default session");
        assert!(config.ws_url().ends_with(&format!("&session={session}")));
        config.validate().expect("the minted session is wire-legal");
    }

    #[test]
    fn validate_refuses_a_session_needing_percent_encoding() {
        let config = MogwaiDataClientConfig {
            base_url: "ws://127.0.0.1:1".into(),
            session: Some("a b".into()),
            ..MogwaiDataClientConfig::default()
        };
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("carried directly")
        );
    }

    #[test]
    fn for_run_binds_the_run_and_names_no_river() {
        let record = ready_record();
        let data = MogwaiDataClientConfig::for_run(&record, AccountId::from(DEFAULT_ACCOUNT_ID));
        let exec = MogwaiExecClientConfig::for_run(&record, AccountId::from(DEFAULT_ACCOUNT_ID));
        assert_eq!(data.symbol, None);
        assert_eq!(exec.symbol, None);
        let data = MogwaiDataClientConfig::for_run(&record, AccountId::from(DEFAULT_ACCOUNT_ID))
            .with_symbol("MNQ");
        let exec = MogwaiExecClientConfig::for_run(&record, AccountId::from(DEFAULT_ACCOUNT_ID))
            .with_symbol("MNQ");
        assert_eq!(data.symbol.as_deref(), Some("MNQ"));
        assert_eq!(exec.symbol.as_deref(), Some("MNQ"));
        assert!(data.ws_url().contains("&symbol=MNQ"));
        assert!(exec.ws_url().contains("&symbol=MNQ"));
    }

    #[test]
    fn validate_refuses_a_symbol_needing_percent_encoding() {
        let config = MogwaiDataClientConfig {
            base_url: "ws://127.0.0.1:1".into(),
            symbol: Some("MN Q".into()),
            ..MogwaiDataClientConfig::default()
        };
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("carried directly")
        );
    }

    #[test]
    fn ws_url_keeps_trimming_a_padded_base_url_with_a_symbol_set() {
        let config = MogwaiExecClientConfig {
            base_url: "  ws://127.0.0.1:1/  ".into(),
            symbol: Some("MNQ".into()),
            session: None,
            ..MogwaiExecClientConfig::default()
        };
        assert_eq!(
            config.ws_url(),
            "ws://127.0.0.1:1/ws?account=MOGWAI-001&symbol=MNQ"
        );
    }
}
