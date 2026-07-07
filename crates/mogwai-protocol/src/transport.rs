use serde::{Deserialize, Serialize};

/// Selects which transport carries order entry and which carries live market
/// data, so one mogwai-server can present itself as different venue archetypes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TransportProfile {
    /// WS carries both order entry and a server-pushed market-data stream.
    #[default]
    WsStreaming,
    /// Order entry over HTTP request/response; market data remains pushed WS.
    HttpOrders,
    /// Order entry over HTTP request/response; market data is polled over HTTP.
    HttpPolling,
}

impl TransportProfile {
    /// Order entry travels over HTTP rather than the `/ws` socket.
    #[must_use]
    pub fn orders_over_http(self) -> bool {
        matches!(self, Self::HttpOrders | Self::HttpPolling)
    }

    /// Live market data is obtained by polling `GET /trades`.
    #[must_use]
    pub fn data_by_polling(self) -> bool {
        matches!(self, Self::HttpPolling)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_profile_round_trips_and_defaults() {
        #[derive(Debug, Deserialize)]
        struct Wrapper {
            #[serde(default)]
            profile: TransportProfile,
        }

        for profile in [
            TransportProfile::WsStreaming,
            TransportProfile::HttpOrders,
            TransportProfile::HttpPolling,
        ] {
            let json = serde_json::to_string(&profile).unwrap();
            let decoded: TransportProfile = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded, profile);
        }

        let decoded: Wrapper = serde_json::from_str("{}").unwrap();
        assert_eq!(decoded.profile, TransportProfile::WsStreaming);

        // orders_over_http: true for both HTTP variants, false only for WS.
        assert!(!TransportProfile::WsStreaming.orders_over_http());
        assert!(TransportProfile::HttpOrders.orders_over_http());
        assert!(TransportProfile::HttpPolling.orders_over_http());

        // data_by_polling: true only for the fully-request/response variant.
        assert!(!TransportProfile::WsStreaming.data_by_polling());
        assert!(!TransportProfile::HttpOrders.data_by_polling());
        assert!(TransportProfile::HttpPolling.data_by_polling());
    }
}
