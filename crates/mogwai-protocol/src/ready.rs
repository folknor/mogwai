// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The one-line record a venue writes to stdout at boot.

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

/// The venue's report to the process that launched it.
///
/// This describes a venue, not a river or boat. A venue can serve many rivers,
/// each carrying at most one boat with its own wall anchor and speed. Nothing
/// that varies per river or boat appears here. Venue identity for attach is
/// `addr` plus `run_seed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadyRecord {
    /// Wire schema version of this record. A launcher reads it FIRST and
    /// refuses a record it does not understand; see [`ReadyRecord::VERSION`].
    pub version: u32,
    pub addr: SocketAddr,
    pub pid: u32,
    /// The value that, with the config, fingerprint and `version_string`,
    /// reproduces every river this venue can serve. Boats are placed at the
    /// venue's fixed placement origin, so a river's path is independent of
    /// when, or whether, anyone boards it.
    pub run_seed: u64,
    /// Earliest `ts_event` any river can serve. The tape origin is identical
    /// for every river, so it remains a venue fact under per-boat clocks.
    pub data_origin_ns: u64,
    /// Placement origin for every boat, whenever it boards, and the epoch from
    /// which `run_duration_ns` is measured. By construction this is
    /// `data_origin_ns + warmup_ns`.
    pub run_start_ns: u64,
    pub run_duration_ns: Option<u64>,
    /// Sim distance from `data_origin_ns` to `run_start_ns`, uniform across
    /// rivers. Every river is servable back to `data_origin_ns`. Materializing
    /// that span is a changing, per-river latency property and is not reported.
    pub warmup_ns: u64,
    pub version_string: String,
}

impl ReadyRecord {
    /// The version this build writes and this build's consumers accept. Bumped
    /// by any landing that adds or changes a field. Stated once, here, so the
    /// venue that writes the record and the test that pins its bytes cannot
    /// disagree about which schema they mean.
    pub const VERSION: u32 = 6;
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
            run_seed: 7,
            data_origin_ns: 1,
            run_start_ns: 2,
            run_duration_ns: None,
            warmup_ns: 1,
            version_string: "test".into(),
        };
        let json = serde_json::to_string(&value).unwrap();
        assert_eq!(
            json,
            r#"{"version":6,"addr":"127.0.0.1:41235","pid":42,"run_seed":7,"data_origin_ns":1,"run_start_ns":2,"run_duration_ns":null,"warmup_ns":1,"version_string":"test"}"#
        );
        assert_eq!(serde_json::from_str::<ReadyRecord>(&json).unwrap(), value);
    }
}
