// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The one-line record a venue writes to its launcher's inherited ready fd.

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

/// Provenance for the generated tape.  A run seed is intentionally not
/// invented here: today the generator derives one seed per symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value")]
pub enum SeedReport {
    PerSymbolFnv(Vec<(String, u64)>),
}

/// The venue's report to the process that launched it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadyRecord {
    /// Wire schema version of this record. A launcher reads it FIRST and
    /// refuses a record it does not understand; see [`ReadyRecord::VERSION`].
    pub version: u32,
    pub addr: SocketAddr,
    pub pid: u32,
    pub symbol: String,
    pub seed: SeedReport,
    pub data_origin_ns: u64,
    pub run_start_ns: u64,
    pub run_duration_ns: Option<u64>,
    pub warmup_ns: u64,
    pub version_string: String,
}

impl ReadyRecord {
    /// The version this build writes and this build's consumers accept. Bumped
    /// by any landing that adds or changes a field. Stated once, here, so the
    /// venue that writes the record and the test that pins its bytes cannot
    /// disagree about which schema they mean.
    pub const VERSION: u32 = 4;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_record_round_trips() {
        let value = ReadyRecord {
            version: ReadyRecord::VERSION,
            addr: "127.0.0.1:41235".parse().unwrap(),
            pid: 42,
            symbol: "BTCUSDT".into(),
            seed: SeedReport::PerSymbolFnv(vec![("BTCUSDT".into(), 7)]),
            data_origin_ns: 1,
            run_start_ns: 2,
            run_duration_ns: None,
            warmup_ns: 1,
            version_string: "test".into(),
        };
        let json = serde_json::to_string(&value).unwrap();
        assert_eq!(
            json,
            r#"{"version":4,"addr":"127.0.0.1:41235","pid":42,"symbol":"BTCUSDT","seed":{"kind":"PerSymbolFnv","value":[["BTCUSDT",7]]},"data_origin_ns":1,"run_start_ns":2,"run_duration_ns":null,"warmup_ns":1,"version_string":"test"}"#
        );
        assert_eq!(serde_json::from_str::<ReadyRecord>(&json).unwrap(), value);
    }
}
