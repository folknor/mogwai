// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

use std::{cell::RefCell, rc::Rc};

use anyhow::anyhow;
use nautilus_common::{
    cache::CacheView,
    clients::{DataClient, ExecutionClient},
    clock::Clock,
    factories::{ClientConfig, DataClientFactory, ExecutionClientFactory},
    live::try_get_exec_event_sender,
};
use nautilus_live::ExecutionClientCore;
use nautilus_model::identifiers::{ClientId, TraderId};

use crate::{
    MOGWAI_VENUE, MOGWAI_VENUE_STR, MogwaiDataClient, MogwaiDataClientConfig,
    MogwaiExecClientConfig, MogwaiExecutionClient,
};

#[derive(Debug, Clone)]
pub struct MogwaiDataClientFactory;

impl MogwaiDataClientFactory {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for MogwaiDataClientFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl DataClientFactory for MogwaiDataClientFactory {
    fn create(
        &self,
        name: &str,
        config: &dyn ClientConfig,
        _cache: CacheView,
        _clock: Rc<RefCell<dyn Clock>>,
    ) -> anyhow::Result<Box<dyn DataClient>> {
        let config = config
            .as_any()
            .downcast_ref::<MogwaiDataClientConfig>()
            .ok_or_else(|| {
                anyhow!(
                    "Invalid config for MogwaiDataClientFactory, expected \
                     MogwaiDataClientConfig, was {config:?}"
                )
            })?
            .clone();

        // No `config.validate()?` here: `MogwaiDataClient::new` validates the
        // config as its first step and propagates any error through the `?`
        // below, so a second validation at the factory is pure redundancy
        // (AD26). The constructor is the single validation site - it is the
        // real entry point, callable directly, not just via this factory - and
        // the factory's own reject tests exercise that path, so a regression
        // that dropped the constructor's check would still turn them red.
        let client = MogwaiDataClient::new(ClientId::from(name), config)?;
        Ok(Box::new(client))
    }

    fn name(&self) -> &'static str {
        MOGWAI_VENUE_STR
    }

    fn config_type(&self) -> &'static str {
        "MogwaiDataClientConfig"
    }
}

#[derive(Debug, Clone)]
pub struct MogwaiExecutionClientFactory;

impl MogwaiExecutionClientFactory {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for MogwaiExecutionClientFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl ExecutionClientFactory for MogwaiExecutionClientFactory {
    fn create(
        &self,
        trader_id: TraderId,
        name: &str,
        config: &dyn ClientConfig,
        cache: CacheView,
    ) -> anyhow::Result<Box<dyn ExecutionClient>> {
        let config = config
            .as_any()
            .downcast_ref::<MogwaiExecClientConfig>()
            .ok_or_else(|| {
                anyhow!(
                    "Invalid config for MogwaiExecutionClientFactory, expected \
                     MogwaiExecClientConfig, was {config:?}"
                )
            })?
            .clone();

        // No `config.validate()?` here: `MogwaiExecutionClient::new` validates
        // the config as its first step and propagates any error through the `?`
        // below (AD26). Building `ExecutionClientCore` first for a config that
        // `new` then rejects only happens on the error path and touches none of
        // the validated fields, so it is harmless. Validating in the
        // constructor keeps a single validation site - see the data factory.
        let core = ExecutionClientCore::new(
            trader_id,
            ClientId::from(name),
            *MOGWAI_VENUE,
            config.oms_type,
            config.account_id,
            config.account_type,
            None,
            cache,
        );
        let mut client = MogwaiExecutionClient::new(core, config)?;
        // Seed the event sink here, and only here, because this is the one site
        // where the runner that owns the thread-local is known. The node binds
        // its senders and then calls this factory in the same function on the
        // same thread - `LiveNodeBuilder::build` does, and so does
        // `LiveNode::build` - so the sender resolved here is that node's.
        //
        // Deliberately not done inside `MogwaiExecutionClient::new`. `new` is
        // public and callable at any moment, and the last runner to bind a
        // thread wins the slot, so a constructor reading it would capture
        // whichever runner happened to own the thread. A client that then ran
        // under a second runner would pass every "is a sender installed" check
        // and deliver its whole event stream to the first - turning a loud,
        // detectable total loss into silent cross-runner misrouting. Absence is
        // catchable; wrong-but-present is not.
        //
        // A `None` here is not fatal: a host constructing clients outside a
        // node still has `start()` on the runner's thread, and `connect()`
        // refuses if that never happened.
        if let Some(sender) = try_get_exec_event_sender() {
            client.install_sink(sender);
        }
        Ok(Box::new(client))
    }

    fn name(&self) -> &'static str {
        MOGWAI_VENUE_STR
    }

    fn config_type(&self) -> &'static str {
        "MogwaiExecClientConfig"
    }
}
