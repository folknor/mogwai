// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Worst-case SERIALIZED-byte bounds on what one `ClientMessage` can make the
//! engine produce. It lives in the protocol crate because it is a statement
//! about the wire format: the server reserves against it before it lets the
//! engine mutate, and the engine's own test suite checks the claim.
//!
//! A finite test matrix samples an upper bound, it cannot prove one, so every
//! constant below carries a field-by-field derivation from the struct it
//! bounds: key names and punctuation counted, each numeric at its widest
//! decimal form, each string at `JSON_ESCAPE_FACTOR` times its cap (a byte
//! serde must escape as `\uXXXX` costs six output bytes, so an ASCII-only
//! measurement would be 6x too small). The fixed addends are scaffolding and
//! numerics only; they are rounded generously upward, because over-reserving
//! costs a connection some budget while under-reserving voids the bound.
use crate::{
    ClientMessage, JSON_ESCAPE_FACTOR, MAX_ACCOUNT_ID_LEN, MAX_CLIENT_ID_LEN, MAX_CURRENCY_LEN,
    MAX_REASON_LEN, MAX_SYMBOL_LEN,
};

/// The engine-state facts a reservation must know to bound a command's output.
/// Read from the engine under the SAME lock that will then process the command,
/// so the shape cannot drift between the reservation and the production it
/// covers.
#[derive(Debug, Clone, Copy)]
pub struct BookShape {
    pub balances: usize,
    pub positions: usize,
    pub open_orders: usize,
    pub closed_orders: usize,
    pub recorded_fills: usize,
}

const ESC: usize = JSON_ESCAPE_FACTOR;

/// Any single order-lifecycle frame (`OrderAccepted`, `OrderRejected`,
/// `OrderCanceled`, `OrderUpdated`, `OrderFilled`, the two modify/cancel
/// rejections). Widest shape is `OrderFilled`: `type`, `client_order_id`,
/// `venue_order_id`, `trade_id`, `symbol`, `side`, `last_qty`, `last_px`,
/// `leaves_qty`, `commission`, `ts_event`. The fixed addend covers ~150 bytes
/// of key names, quotes, colons, commas and braces, four `Decimal`s at their
/// widest serialized form (~33 bytes each, 132), a u64 at 20, a `Side`
/// spelling at 5, and the server-generated `trade_id` and its key (~35) - about
/// 350, rounded to 512. Charged separately: two client-id-shaped strings (the
/// client id, and the venue id which is server-generated and shorter), one
/// symbol and one reason.
pub const ORDER_EVENT_MAX_BYTES: usize =
    512 + ESC * (2 * MAX_CLIENT_ID_LEN + MAX_SYMBOL_LEN + MAX_REASON_LEN);

/// One `Balance` row inside `AccountState`: `currency`, `total`, `free`,
/// `locked` - three decimals at ~32 bytes plus ~60 bytes of key names,
/// punctuation and the enclosing braces, rounded to 192.
pub const BALANCE_ROW_MAX_BYTES: usize = 192 + ESC * MAX_CURRENCY_LEN;

/// One `Position` row inside `AccountState`: `symbol`, `quantity`,
/// `avg_px` - two decimals at ~32 bytes plus key names and punctuation,
/// rounded to 128.
pub const POSITION_ROW_MAX_BYTES: usize = 128 + ESC * MAX_SYMBOL_LEN;

/// One `OrderStatusInfo` row inside an `OrderStatusSnapshot`:
/// `client_order_id`, `venue_order_id`, `symbol`, `side`, `order_type`,
/// `time_in_force`, `status`, `quantity`, `filled_qty`, `price`,
/// `trigger_price`, `ts_triggered`, `reduce_only`, `post_only`, `ts_accepted`,
/// `ts_last` - ~180 bytes of key names and punctuation, four decimals (132),
/// three u64s (60), four short enum spellings (~40) and two bools (10): about
/// 430, rounded to 512 on top of the charged strings.
pub const ORDER_STATUS_ROW_MAX_BYTES: usize = 512 + ESC * (2 * MAX_CLIENT_ID_LEN + MAX_SYMBOL_LEN);

/// One fill row inside a `FillSnapshot`: an `OrderFilled` plus its trade id, so
/// three client-id-shaped strings (client, venue, trade), one symbol, four
/// decimals, a u64 and two enum spellings, rounded to 320.
pub const FILL_ROW_MAX_BYTES: usize = 320 + ESC * (3 * MAX_CLIENT_ID_LEN + MAX_SYMBOL_LEN);

/// The envelope either snapshot wraps its rows in: `type`, `request_id`, the
/// row-array brackets and `ts_event`, rounded to 128 plus the echoed
/// `request_id` (capped by `validate_request_id` at `MAX_CLIENT_ID_LEN`).
pub const SNAPSHOT_ENVELOPE_MAX_BYTES: usize = 128 + ESC * MAX_CLIENT_ID_LEN;

/// A protocol-boundary refusal produces exactly one order-shaped frame and no
/// `AccountState`, so its worst case is a constant - which is what lets the two
/// pre-engine refusal paths reserve without a `BookShape` to size against.
pub const BOUNDARY_REFUSAL_BYTES: usize = ORDER_EVENT_MAX_BYTES;

/// Upper bound on one serialized `AccountState`: the envelope plus every
/// balance and position row the book currently carries.
#[must_use]
pub fn account_state_max_bytes(shape: &BookShape) -> usize {
    144 + ESC * MAX_ACCOUNT_ID_LEN
        + shape.balances * BALANCE_ROW_MAX_BYTES
        + shape.positions * POSITION_ROW_MAX_BYTES
}

/// Upper bound on one trigger sweep's output: per executed order, up to FOUR
/// order-shaped frames - `OrderTriggered`, the fill, its possible
/// `DuplicateNextFill` twin, and the `OrderCanceled` that closes a reduce-only
/// remainder the position cap clamped - and ONE `AccountState` for the whole
/// batch (the sweep snapshots once, after every transition it booked).
///
/// The account is sized against a shape widened PER ORDER, not per batch: a
/// sweep can execute `orders` fills across `orders` distinct pairs, and each
/// first fill in a new pair introduces up to two currencies and one position the
/// pre-sweep snapshot never had. Widening by a flat `+2/+1` (the single-command
/// `SubmitOrder` case) under-reserves any multi-symbol batch, which is exactly
/// the domination failure the held-byte budget exists to prevent.
///
/// `orders` is the count of orders the sweep actually EMITS for, never the count
/// of pending scans: a scan below its threshold produces no bytes.
#[must_use]
pub fn swept_fill_max_bytes(shape: &BookShape, orders: usize) -> usize {
    orders * 4 * ORDER_EVENT_MAX_BYTES
        + account_state_max_bytes(&BookShape {
            balances: shape.balances + 2 * orders,
            positions: shape.positions + orders,
            ..*shape
        })
}

/// Upper bound on the total serialized bytes `Engine::process` can produce for
/// `cmd` against a book of `shape`. The worst cases are enumerated from the
/// engine's own branches and pinned by
/// `worst_case_reservation_covers_actual_output` in `mogwai-engine`, which
/// samples the claim the derivations above argue.
#[must_use]
pub fn worst_case_output_bytes(cmd: &ClientMessage, shape: &BookShape) -> usize {
    match cmd {
        // Five order-shaped frames - accepted, the trigger, a duplicated fill,
        // the fill, and the cancel that closes the remainder - plus one account
        // state. Four was the pre-conditional bound (accepted, duplicated fill,
        // fill, canceled IOC remainder); an arrival-triggered reduce-only stop
        // whose fill the position cap clamps adds the trigger on top of exactly
        // that shape, and it cannot also be an IOC because a conditional is
        // GTC-only. The account is
        // sized against a WIDENED shape: a fill mutates both the base and the
        // quote entry via `entry(..).or_default()`, so a first fill in a new
        // pair introduces up to two currencies and one position the pre-command
        // snapshot never had. Widening by less under-counts by up to two
        // balance rows and makes the domination claim false.
        ClientMessage::SubmitOrder(_) => {
            5 * ORDER_EVENT_MAX_BYTES
                + account_state_max_bytes(&BookShape {
                    balances: shape.balances + 2,
                    positions: shape.positions + 1,
                    ..*shape
                })
        }
        // One order event (the cancel/update, or its rejection) plus the
        // account state that follows a book mutation.
        ClientMessage::CancelOrder { .. } | ClientMessage::ModifyOrder { .. } => {
            ORDER_EVENT_MAX_BYTES + account_state_max_bytes(shape)
        }
        ClientMessage::QueryOrders { .. } => {
            SNAPSHOT_ENVELOPE_MAX_BYTES
                + (shape.open_orders + shape.closed_orders) * ORDER_STATUS_ROW_MAX_BYTES
        }
        ClientMessage::QueryFills { .. } => {
            SNAPSHOT_ENVELOPE_MAX_BYTES + shape.recorded_fills * FILL_ROW_MAX_BYTES
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AccountId, AccountState};

    #[test]
    fn account_state_bound_covers_a_max_length_account_id() {
        let shape = BookShape {
            balances: 0,
            positions: 0,
            open_orders: 0,
            closed_orders: 0,
            recorded_fills: 0,
        };
        let state = AccountState {
            account_id: AccountId::parse(&"Z".repeat(MAX_ACCOUNT_ID_LEN)).unwrap(),
            balances: Vec::new(),
            positions: Vec::new(),
            ts_event: u64::MAX,
        };
        assert!(
            account_state_max_bytes(&shape) >= serde_json::to_vec(&state).unwrap().len(),
            "account snapshot bound must dominate its wire bytes"
        );
    }
}
