use async_trait::async_trait;
use nautilus_common::clients::{DataClient, ExecutionClient};
use nautilus_core::UnixNanos;
use nautilus_live::ExecutionClientCore;
use nautilus_model::{
    accounts::AccountAny,
    enums::OmsType,
    identifiers::{AccountId, ClientId, Venue},
    types::{AccountBalance, MarginBalance},
};

use crate::{MOGWAI_VENUE, MogwaiDataClientConfig, MogwaiExecClientConfig};

#[derive(Debug)]
#[allow(dead_code)]
pub struct MogwaiDataClient {
    client_id: ClientId,
    config: MogwaiDataClientConfig,
    connected: bool,
}

impl MogwaiDataClient {
    /// Creates a new disconnected mogwai data client.
    ///
    /// # Errors
    ///
    /// Returns an error if the supplied config is invalid.
    pub fn new(client_id: ClientId, config: MogwaiDataClientConfig) -> anyhow::Result<Self> {
        config.validate()?;
        Ok(Self {
            client_id,
            config,
            connected: false,
        })
    }
}

#[async_trait(?Send)]
impl DataClient for MogwaiDataClient {
    fn client_id(&self) -> ClientId {
        self.client_id
    }

    fn venue(&self) -> Option<Venue> {
        Some(*MOGWAI_VENUE)
    }

    fn start(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    fn stop(&mut self) -> anyhow::Result<()> {
        self.connected = false;
        Ok(())
    }

    fn reset(&mut self) -> anyhow::Result<()> {
        self.connected = false;
        Ok(())
    }

    fn dispose(&mut self) -> anyhow::Result<()> {
        self.stop()
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    fn is_disconnected(&self) -> bool {
        !self.connected
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct MogwaiExecutionClient {
    core: ExecutionClientCore,
    config: MogwaiExecClientConfig,
}

impl MogwaiExecutionClient {
    /// Creates a new disconnected mogwai execution client.
    ///
    /// # Errors
    ///
    /// Returns an error if the supplied config is invalid.
    pub fn new(core: ExecutionClientCore, config: MogwaiExecClientConfig) -> anyhow::Result<Self> {
        config.validate()?;
        Ok(Self { core, config })
    }

    #[cfg(test)]
    fn is_started(&self) -> bool {
        self.core.is_started()
    }

    #[cfg(test)]
    fn is_stopped(&self) -> bool {
        self.core.is_stopped()
    }
}

#[async_trait(?Send)]
impl ExecutionClient for MogwaiExecutionClient {
    fn is_connected(&self) -> bool {
        self.core.is_connected()
    }

    fn client_id(&self) -> ClientId {
        self.core.client_id
    }

    fn account_id(&self) -> AccountId {
        self.core.account_id
    }

    fn venue(&self) -> Venue {
        *MOGWAI_VENUE
    }

    fn oms_type(&self) -> OmsType {
        self.core.oms_type
    }

    fn get_account(&self) -> Option<AccountAny> {
        self.core.cache().account_owned(&self.core.account_id)
    }

    fn generate_account_state(
        &self,
        _balances: Vec<AccountBalance>,
        _margins: Vec<MarginBalance>,
        _reported: bool,
        _ts_event: UnixNanos,
    ) -> anyhow::Result<()> {
        // The trait requires this surface before account snapshot emission is
        // implemented. Later handlers will consume mogwai AccountState messages
        // and emit real nautilus account events here.
        Ok(())
    }

    fn start(&mut self) -> anyhow::Result<()> {
        if self.core.is_started() {
            return Ok(());
        }

        self.core.set_started();
        Ok(())
    }

    fn stop(&mut self) -> anyhow::Result<()> {
        if self.core.is_stopped() {
            return Ok(());
        }

        self.core.set_stopped();
        self.core.set_disconnected();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use nautilus_common::{cache::Cache, clients::ExecutionClient};
    use nautilus_live::ExecutionClientCore;
    use nautilus_model::{
        enums::OmsType,
        identifiers::{ClientId, TraderId},
    };

    use super::*;

    fn execution_client() -> MogwaiExecutionClient {
        let config = MogwaiExecClientConfig::default();
        let cache = Rc::new(RefCell::new(Cache::default()));
        let core = ExecutionClientCore::new(
            TraderId::from("MOGWAI-001"),
            ClientId::from("MOGWAI-TEST"),
            *MOGWAI_VENUE,
            OmsType::Netting,
            config.account_id,
            config.account_type,
            None,
            cache,
        );

        MogwaiExecutionClient::new(core, config).expect("valid execution client")
    }

    #[test]
    fn mogwai_exec_client_start_stop_are_idempotent() {
        let mut client = execution_client();

        assert!(client.is_stopped());
        assert!(!client.is_started());

        client.start().expect("first start succeeds");
        client.start().expect("second start succeeds");
        assert!(client.is_started());
        assert!(!client.is_stopped());

        client.stop().expect("first stop succeeds");
        client.stop().expect("second stop succeeds");
        assert!(client.is_stopped());
        assert!(!client.is_started());
        assert!(!client.is_connected());
    }
}
