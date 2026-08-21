// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The one-line record a venue writes to stdout at boot.

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

/// The venue's report to the process that launched it.
///
/// This describes a venue, not a river or boat. A venue can serve many rivers,
/// each carrying as many boats as distinct speeds, each with its own wall
/// anchor. Nothing
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
    /// Whether a consumer returning with an account id it has already traded is
    /// handed a CLEAN ledger instead of its own.
    ///
    /// FALSE means accounts PERSIST across reconnection, which is the default
    /// and is what makes a reconnect a continuation rather than a new trader.
    /// That is a behaviour an operator must be told about rather than discover:
    /// a run that silently inherits a stale position book is measuring something
    /// nobody asked for.
    ///
    /// ON THE RECORD RATHER THAN IN A LOG LINE, deliberately. The launcher
    /// already parses this line, so a consumer can assert on the setting instead
    /// of a human having to notice a message at boot.
    pub reset_account_on_reconnect: bool,
    /// How long an UNATTENDED account survives before the venue collects it, in
    /// wall milliseconds. `0` means never.
    ///
    /// An account whose last connection went away is FROZEN, not liquidated: it
    /// is not swept, not marked and not judged, and a socket returning with the
    /// same id resumes it. That is what lets a killed worker come back to its
    /// own book - and it is also state with no lifecycle, so a span bounds it.
    ///
    /// On the record for the same reason as the setting above: a consumer whose
    /// restart takes longer than this gets a clean ledger rather than its
    /// positions back, and that is a fact it must be able to assert on rather
    /// than discover.
    pub account_ttl_ms: u64,
    pub version_string: String,
}

impl ReadyRecord {
    /// The version this build writes and this build's consumers accept. Bumped
    /// by any landing that adds or changes a field. Stated once, here, so the
    /// venue that writes the record and the test that pins its bytes cannot
    /// disagree about which schema they mean.
    pub const VERSION: u32 = 8;
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
            reset_account_on_reconnect: false,
            account_ttl_ms: 0,
            version_string: "test".into(),
        };
        let json = serde_json::to_string(&value).unwrap();
        assert_eq!(
            json,
            r#"{"version":8,"addr":"127.0.0.1:41235","pid":42,"run_seed":7,"data_origin_ns":1,"run_start_ns":2,"run_duration_ns":null,"warmup_ns":1,"reset_account_on_reconnect":false,"account_ttl_ms":0,"version_string":"test"}"#
        );
        assert_eq!(serde_json::from_str::<ReadyRecord>(&json).unwrap(), value);
    }
}
