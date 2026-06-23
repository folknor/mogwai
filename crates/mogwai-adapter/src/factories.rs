use std::{cell::RefCell, rc::Rc};

use anyhow::anyhow;
use nautilus_common::{
    cache::CacheView,
    clients::{DataClient, ExecutionClient},
    clock::Clock,
    factories::{ClientConfig, DataClientFactory, ExecutionClientFactory},
};
use nautilus_live::ExecutionClientCore;
use nautilus_model::identifiers::ClientId;

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
        config.validate()?;

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
        config.validate()?;

        let core = ExecutionClientCore::new(
            config.trader_id,
            ClientId::from(name),
            *MOGWAI_VENUE,
            config.oms_type,
            config.account_id,
            config.account_type,
            None,
            cache,
        );
        let client = MogwaiExecutionClient::new(core, config)?;
        Ok(Box::new(client))
    }

    fn name(&self) -> &'static str {
        MOGWAI_VENUE_STR
    }

    fn config_type(&self) -> &'static str {
        "MogwaiExecClientConfig"
    }
}

#[cfg(test)]
mod tests {
    use std::{any::Any, cell::RefCell, rc::Rc};

    use mogwai_protocol::{ClientHavoc, HavocLatency, HavocSpec, MarketRegime, TransportProfile};
    use nautilus_common::{
        cache::Cache,
        clock::TestClock,
        factories::{ClientConfig, DataClientFactory, ExecutionClientFactory},
    };
    use nautilus_model::{
        enums::OmsType,
        identifiers::{ClientId, TraderId},
    };

    use super::*;

    #[derive(Debug)]
    struct ForeignConfig;

    impl ClientConfig for ForeignConfig {
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    fn cache() -> Rc<RefCell<Cache>> {
        Rc::new(RefCell::new(Cache::default()))
    }

    fn havoc_spec() -> HavocSpec {
        HavocSpec {
            client: ClientHavoc {
                latency: Some(HavocLatency {
                    base_nanos: 10,
                    exec_event_nanos: 20,
                    fill_nanos: 30,
                    data_nanos: 40,
                }),
                drop_prob: 0.1,
                duplicate_prob: 0.2,
                reorder_prob: 0.3,
                seed: Some(7),
            },
            server: Vec::new(),
            data: None,
            conn: Default::default(),
        }
    }

    #[test]
    fn mogwai_data_factory_reports_name_and_config_type() {
        let factory = MogwaiDataClientFactory::new();

        assert_eq!(factory.name(), "MOGWAI");
        assert_eq!(factory.config_type(), "MogwaiDataClientConfig");
    }

    #[test]
    fn mogwai_exec_factory_reports_name_and_config_type() {
        let factory = MogwaiExecutionClientFactory::new();

        assert_eq!(factory.name(), "MOGWAI");
        assert_eq!(factory.config_type(), "MogwaiExecClientConfig");
    }

    #[test]
    fn mogwai_data_factory_creates_client_with_client_id() {
        let factory = MogwaiDataClientFactory::new();
        let config = MogwaiDataClientConfig::default();
        let cache = cache();
        let clock = Rc::new(RefCell::new(TestClock::new()));

        let client = factory
            .create("MOGWAI-TEST", &config, cache.into(), clock)
            .expect("factory creates data client");

        assert_eq!(client.client_id(), ClientId::from("MOGWAI-TEST"));
    }

    #[test]
    fn mogwai_exec_factory_creates_client_with_netting_oms() {
        let factory = MogwaiExecutionClientFactory::new();
        let config = MogwaiExecClientConfig {
            trader_id: TraderId::from("MOGWAI-001"),
            ..Default::default()
        };
        let cache = cache();

        let client = factory
            .create("MOGWAI-TEST", &config, cache.into())
            .expect("factory creates execution client");

        assert_eq!(client.client_id(), ClientId::from("MOGWAI-TEST"));
        assert_eq!(client.account_id(), config.account_id);
        assert_eq!(client.oms_type(), OmsType::Netting);
    }

    #[test]
    fn mogwai_exec_factory_threads_configured_oms_through_to_client() {
        // D.7: the OMS type was hard-coded to `Netting`; broadarrow must be able
        // to override it per-venue. A config that asks for `Hedging` reaches the
        // exec client core unchanged.
        let factory = MogwaiExecutionClientFactory::new();
        let config = MogwaiExecClientConfig {
            oms_type: OmsType::Hedging,
            ..Default::default()
        };
        let cache = cache();

        let client = factory
            .create("MOGWAI-TEST", &config, cache.into())
            .expect("factory creates execution client");

        assert_eq!(client.oms_type(), OmsType::Hedging);
    }

    #[test]
    fn mogwai_factory_rejects_wrong_config_type() {
        let wrong_config = ForeignConfig;
        let data_factory = MogwaiDataClientFactory::new();
        let exec_factory = MogwaiExecutionClientFactory::new();
        let cache = cache();
        let clock = Rc::new(RefCell::new(TestClock::new()));

        let data_err = match data_factory.create(
            "MOGWAI-TEST",
            &wrong_config,
            Rc::clone(&cache).into(),
            clock,
        ) {
            Ok(_) => panic!("data factory accepted foreign config"),
            Err(error) => error.to_string(),
        };
        let exec_err = match exec_factory.create("MOGWAI-TEST", &wrong_config, cache.into()) {
            Ok(_) => panic!("execution factory accepted foreign config"),
            Err(error) => error.to_string(),
        };

        assert!(data_err.contains("Invalid config"));
        assert!(exec_err.contains("Invalid config"));
    }

    #[test]
    fn mogwai_factory_rejects_empty_base_url() {
        // `create` runs `config.validate()?` after the downcast; pin that a
        // blank mogwai-server URL is rejected so a misconfigured client never
        // reaches the later transport landing with nowhere to connect.
        let data_factory = MogwaiDataClientFactory::new();
        let data_config = MogwaiDataClientConfig {
            base_url: "   ".to_string(),
            ..MogwaiDataClientConfig::default()
        };
        let cache = cache();
        let clock = Rc::new(RefCell::new(TestClock::new()));

        let data_err =
            match data_factory.create("MOGWAI-TEST", &data_config, Rc::clone(&cache).into(), clock)
            {
                Ok(_) => panic!("data factory accepted empty base_url"),
                Err(error) => error.to_string(),
            };

        let exec_factory = MogwaiExecutionClientFactory::new();
        let exec_config = MogwaiExecClientConfig {
            base_url: String::new(),
            ..Default::default()
        };
        let exec_err = match exec_factory.create("MOGWAI-TEST", &exec_config, cache.into()) {
            Ok(_) => panic!("execution factory accepted empty base_url"),
            Err(error) => error.to_string(),
        };

        assert!(data_err.contains("base_url"));
        assert!(exec_err.contains("base_url"));
    }

    #[test]
    fn mogwai_configs_round_trip_json() {
        let data_config = MogwaiDataClientConfig {
            base_url: "ws://example.invalid:9999".to_string(),
            transport_profile: TransportProfile::HttpPolling,
            havoc: Some(havoc_spec()),
        };
        let data_json = serde_json::to_string(&data_config).expect("serialize data client config");
        let data_round_trip: MogwaiDataClientConfig =
            serde_json::from_str(&data_json).expect("deserialize data client config");
        assert_eq!(data_round_trip.base_url, data_config.base_url);
        assert_eq!(
            data_round_trip.transport_profile,
            data_config.transport_profile
        );
        assert_eq!(data_round_trip.havoc, data_config.havoc);

        let exec_config = MogwaiExecClientConfig {
            trader_id: TraderId::from("MOGWAI-042"),
            base_url: "ws://example.invalid:9999".to_string(),
            transport_profile: TransportProfile::HttpOrders,
            havoc: Some(havoc_spec()),
            ..Default::default()
        };
        let exec_json =
            serde_json::to_string(&exec_config).expect("serialize execution client config");
        let exec_round_trip: MogwaiExecClientConfig =
            serde_json::from_str(&exec_json).expect("deserialize execution client config");
        assert_eq!(exec_round_trip.trader_id, exec_config.trader_id);
        assert_eq!(exec_round_trip.account_id, exec_config.account_id);
        assert_eq!(exec_round_trip.base_url, exec_config.base_url);
        assert_eq!(exec_round_trip.account_type, exec_config.account_type);
        assert_eq!(
            exec_round_trip.transport_profile,
            exec_config.transport_profile
        );
        assert_eq!(exec_round_trip.havoc, exec_config.havoc);
    }

    #[test]
    fn mogwai_configs_fill_missing_fields_from_default() {
        // The broadarrow config-file path deserializes possibly-partial tables;
        // `#[serde(default)]` on each config wires the `Default` impl into
        // deserialization so a document that omits a field is filled rather
        // than rejected. Pin that the omitted fields fall back to the defaults.
        let data: MogwaiDataClientConfig =
            serde_json::from_str("{}").expect("partial data config deserializes");
        assert_eq!(data.base_url, MogwaiDataClientConfig::default().base_url);
        assert_eq!(data.transport_profile, TransportProfile::WsStreaming);
        assert_eq!(data.havoc, None);

        let exec: MogwaiExecClientConfig =
            serde_json::from_str(r#"{"base_url":"ws://example.invalid:9999"}"#)
                .expect("partial exec config deserializes");
        let defaults = MogwaiExecClientConfig::default();
        assert_eq!(exec.base_url, "ws://example.invalid:9999");
        assert_eq!(exec.trader_id, defaults.trader_id);
        assert_eq!(exec.account_id, defaults.account_id);
        assert_eq!(exec.account_type, defaults.account_type);
        assert_eq!(exec.transport_profile, TransportProfile::WsStreaming);
        assert_eq!(exec.havoc, None);
    }

    #[test]
    fn mogwai_config_rejects_out_of_range_probability() {
        let data_factory = MogwaiDataClientFactory::new();
        let cache = cache();
        let clock = Rc::new(RefCell::new(TestClock::new()));
        let data_config = MogwaiDataClientConfig {
            havoc: Some(HavocSpec {
                client: ClientHavoc {
                    drop_prob: 1.1,
                    ..ClientHavoc::default()
                },
                server: Vec::new(),
                data: None,
                conn: Default::default(),
            }),
            ..MogwaiDataClientConfig::default()
        };

        let data_err =
            match data_factory.create("MOGWAI-TEST", &data_config, Rc::clone(&cache).into(), clock)
            {
                Ok(_) => panic!("data factory accepted invalid havoc probability"),
                Err(error) => error.to_string(),
            };

        let exec_factory = MogwaiExecutionClientFactory::new();
        let exec_config = MogwaiExecClientConfig {
            havoc: Some(HavocSpec {
                client: ClientHavoc {
                    reorder_prob: -0.1,
                    ..ClientHavoc::default()
                },
                server: Vec::new(),
                data: None,
                conn: Default::default(),
            }),
            ..MogwaiExecClientConfig::default()
        };
        let exec_err = match exec_factory.create("MOGWAI-TEST", &exec_config, cache.into()) {
            Ok(_) => panic!("execution factory accepted invalid havoc probability"),
            Err(error) => error.to_string(),
        };

        assert!(data_err.contains("drop_prob"));
        assert!(exec_err.contains("reorder_prob"));
    }

    #[test]
    fn mogwai_config_rejects_out_of_range_data_regime() {
        let data_factory = MogwaiDataClientFactory::new();
        let cache = cache();
        let clock = Rc::new(RefCell::new(TestClock::new()));
        let data_config = MogwaiDataClientConfig {
            havoc: Some(HavocSpec {
                data: Some(MarketRegime::LiquidityDrought { thin_factor: 0.5 }),
                ..HavocSpec::default()
            }),
            ..MogwaiDataClientConfig::default()
        };

        let data_err =
            match data_factory.create("MOGWAI-TEST", &data_config, Rc::clone(&cache).into(), clock)
            {
                Ok(_) => panic!("data factory accepted invalid data regime"),
                Err(error) => error.to_string(),
            };

        let exec_factory = MogwaiExecutionClientFactory::new();
        let exec_config = MogwaiExecClientConfig {
            havoc: Some(HavocSpec {
                data: Some(MarketRegime::VolStorm { vol_mult: 0.0 }),
                ..HavocSpec::default()
            }),
            ..MogwaiExecClientConfig::default()
        };
        let exec_err = match exec_factory.create("MOGWAI-TEST", &exec_config, cache.into()) {
            Ok(_) => panic!("execution factory accepted invalid data regime"),
            Err(error) => error.to_string(),
        };

        assert!(data_err.contains("thin_factor"));
        assert!(exec_err.contains("vol_mult"));
    }

    #[test]
    fn mogwai_config_rejects_out_of_range_conn_havoc() {
        let data_factory = MogwaiDataClientFactory::new();
        let cache = cache();
        let clock = Rc::new(RefCell::new(TestClock::new()));
        let data_config = MogwaiDataClientConfig {
            havoc: Some(HavocSpec {
                conn: mogwai_protocol::ConnHavoc {
                    reconnect_backoff_factor: 0.5,
                    ..Default::default()
                },
                ..HavocSpec::default()
            }),
            ..MogwaiDataClientConfig::default()
        };

        let data_err =
            match data_factory.create("MOGWAI-TEST", &data_config, Rc::clone(&cache).into(), clock)
            {
                Ok(_) => panic!("data factory accepted invalid connection havoc"),
                Err(error) => error.to_string(),
            };

        let exec_factory = MogwaiExecutionClientFactory::new();
        let exec_config = MogwaiExecClientConfig {
            havoc: Some(HavocSpec {
                conn: mogwai_protocol::ConnHavoc {
                    max_requests_per_second: Some(0),
                    ..Default::default()
                },
                ..HavocSpec::default()
            }),
            ..MogwaiExecClientConfig::default()
        };
        let exec_err = match exec_factory.create("MOGWAI-TEST", &exec_config, cache.into()) {
            Ok(_) => panic!("execution factory accepted invalid connection havoc"),
            Err(error) => error.to_string(),
        };

        assert!(data_err.contains("reconnect_backoff_factor"));
        assert!(exec_err.contains("max_requests_per_second"));
    }
}
