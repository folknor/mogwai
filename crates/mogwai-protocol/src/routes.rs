// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Native HTTP and WebSocket route paths shared by the venue and its adapter.

pub const HEALTH: &str = "/health";
pub const ACCOUNT: &str = "/account";
pub const INSTRUMENTS: &str = "/instruments";
pub const OPERATOR_TRADES: &str = "/operator/trades";
pub const OPERATOR_QUOTES: &str = "/operator/quotes";
pub const CLOCK: &str = "/clock";
pub const WS: &str = "/ws";
pub const ACCOUNTS: &str = "/accounts";
pub const CONTROL_DIVERGENCE: &str = "/control/divergence";

/// A route path without its leading slash, for URL joiners that insert it.
#[must_use]
pub fn segment(path: &str) -> &str {
    path.strip_prefix('/').unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_segments_are_the_registered_route_bytes_without_the_slash() {
        assert_eq!(segment(ACCOUNT), "account");
        assert_eq!(segment(CLOCK), "clock");
        assert_eq!(segment(CONTROL_DIVERGENCE), "control/divergence");
    }

    #[test]
    fn registered_route_bytes_are_unchanged() {
        assert_eq!(
            [
                HEALTH,
                ACCOUNT,
                INSTRUMENTS,
                OPERATOR_TRADES,
                OPERATOR_QUOTES,
                CLOCK,
                WS,
                ACCOUNTS,
                CONTROL_DIVERGENCE,
            ],
            [
                "/health",
                "/account",
                "/instruments",
                "/operator/trades",
                "/operator/quotes",
                "/clock",
                "/ws",
                "/accounts",
                "/control/divergence",
            ]
        );
    }
}
