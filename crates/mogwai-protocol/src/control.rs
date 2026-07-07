//! Out-of-band control plane: arm deterministic divergences for tests.
//!
//! This is the reason mogwai exists as an external process - it can emit ugly,
//! realistic event streams an in-process matching engine never would, to drive
//! broadarrow's `classify` → brake/quarantine/restart layer.

use crate::{ClientOrderId, Decimal, Deserialize, Serialize};

/// Upper bound on any single divergence's `ms` window, enforced by
/// `validate_divergence`.
///
/// One hour is far longer than any test blackout, data-stall, or ack-delay
/// scenario needs, and `3_600_000 * 1_000_000` ns is well below `u64::MAX`,
/// so validated temporal windows cannot saturate writer deadlines.
pub const MAX_DIVERGENCE_MS: u64 = 3_600_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Divergence {
    /// Fill the next matching order only `fraction` of the way, leaving the rest open.
    PartialFillNext {
        client_order_id: ClientOrderId,
        fraction: Decimal,
    },
    /// Reject the next submitted order with `reason`.
    RejectNextSubmit { reason: String },
    /// Delay every outbound execution event by `ms`, bounded by
    /// `MAX_DIVERGENCE_MS`. Arm with `ms: 0` to clear, or post
    /// `ClearDivergences`.
    DelayAcks { ms: u64 },
    /// Emit the next fill event twice.
    DuplicateNextFill,
    /// Swallow the next fill-driven account-state update (induce account drift).
    DropNextAccountUpdate,
    /// Stop sending anything for `ms` (simulate a venue blackout), bounded
    /// by `MAX_DIVERGENCE_MS`. Frames produced during the window are
    /// dropped, not buffered. Post `ClearDivergences` to lift the window
    /// early.
    GoDark { ms: u64 },
    /// Suppress only market-data frames (`Trade` / `Quote`) for `ms`,
    /// leaving every execution frame alive. Bounded by
    /// `MAX_DIVERGENCE_MS`. Frames produced during the window are dropped,
    /// not buffered. Post `ClearDivergences` to lift the window early.
    ///
    /// Unlike `GoDark`, this keeps the socket healthy while only channel
    /// data is withheld, especially when paired with the server
    /// `Heartbeat`.
    StallData { ms: u64 },
    /// Clear the server-owned temporal windows: cancel any armed
    /// `DelayAcks`, any armed `GoDark`, and any armed `StallData`.
    ///
    /// This does not flush engine-side single-shot divergences
    /// (`PartialFillNext`, `RejectNextSubmit`, `DuplicateNextFill`,
    /// `DropNextAccountUpdate`), which self-disarm on their own trigger.
    ClearDivergences,
}
