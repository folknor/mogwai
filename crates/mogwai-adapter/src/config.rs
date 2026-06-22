use std::any::Any;

use anyhow::ensure;
use nautilus_common::factories::ClientConfig;
use nautilus_model::{
    enums::AccountType,
    identifiers::{AccountId, TraderId},
};
use serde::{Deserialize, Serialize};

const DEFAULT_BASE_URL: &str = "ws://127.0.0.1:8787";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MogwaiDataClientConfig {
    /// Base URL of the running mogwai-server.
    ///
    /// Later data handlers derive the `/ws` market-data path from this value.
    /// The skeleton stores and validates the URL without opening a transport.
    pub base_url: String,
}

impl Default for MogwaiDataClientConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }
}

impl MogwaiDataClientConfig {
    /// Validates config invariants.
    ///
    /// # Errors
    ///
    /// Returns an error if the mogwai-server URL is empty.
    pub fn validate(&self) -> anyhow::Result<()> {
        ensure!(!self.base_url.trim().is_empty(), "base_url cannot be empty");
        Ok(())
    }

    pub fn ws_url(&self) -> String {
        self.base_url.clone()
    }

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
}

impl Default for MogwaiExecClientConfig {
    fn default() -> Self {
        Self {
            trader_id: TraderId::from("MOGWAI-001"),
            account_id: AccountId::from("MOGWAI-001"),
            base_url: DEFAULT_BASE_URL.to_string(),
            account_type: AccountType::Cash,
        }
    }
}

impl MogwaiExecClientConfig {
    /// Validates config invariants.
    ///
    /// # Errors
    ///
    /// Returns an error if the mogwai-server URL is empty.
    pub fn validate(&self) -> anyhow::Result<()> {
        ensure!(!self.base_url.trim().is_empty(), "base_url cannot be empty");
        Ok(())
    }

    pub fn ws_url(&self) -> String {
        self.base_url.clone()
    }

    pub fn http_base_url(&self) -> String {
        http_base_url(&self.base_url)
    }
}

impl ClientConfig for MogwaiExecClientConfig {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn http_base_url(base_url: &str) -> String {
    if let Some(rest) = base_url.strip_prefix("ws://") {
        format!("http://{rest}")
    } else if let Some(rest) = base_url.strip_prefix("wss://") {
        format!("https://{rest}")
    } else {
        base_url.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_base_url_uses_server_port() {
        let cfg = MogwaiDataClientConfig::default();

        assert_eq!(cfg.ws_url(), "ws://127.0.0.1:8787");
        assert_eq!(cfg.http_base_url(), "http://127.0.0.1:8787");
    }

    #[test]
    fn http_base_url_normalizes_secure_ws_scheme() {
        let cfg = MogwaiDataClientConfig {
            base_url: "wss://example.test:9443".into(),
        };

        assert_eq!(cfg.ws_url(), "wss://example.test:9443");
        assert_eq!(cfg.http_base_url(), "https://example.test:9443");
    }
}
