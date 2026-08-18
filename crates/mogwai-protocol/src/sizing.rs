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
    MAX_LINKED_ORDERS, MAX_SYMBOL_LEN,
};

/// The engine-state facts a reservation must know to bound a command's output.
/// Read from the engine under the SAME lock that will then process the command,
/// so the shape cannot drift between the reservation and the production it
/// covers.
#[derive(Debug, Clone, Copy)]
pub struct BookShape {
    pub balances: usize,
    pub positions: usize,
    pub margins: usize,
    pub open_orders: usize,
    pub closed_orders: usize,
    pub recorded_fills: usize,
}

const ESC: usize = JSON_ESCAPE_FACTOR;

/// Any single order-lifecycle frame (`OrderAccepted`, `OrderRejected`,
/// `OrderCanceled`, `OrderExpired`, `OrderUpdated`, `OrderFilled`, the two
/// modify/cancel rejections). `OrderExpired` carries `OrderCanceled`'s exact
/// three fields, so it needs no bound of its own. Widest shape is
/// `OrderFilled`: `type`, `client_order_id`,
/// `venue_order_id`, `trade_id`, `position_id`, `symbol`, `side`, `last_qty`,
/// `last_px`, `leaves_qty`, `commission`, `currency`, `liquidity`, `ts_event`.
/// The fixed addend covers ~190 bytes
/// of key names, quotes, colons, commas and braces, four `Decimal`s at their
/// widest serialized form (~33 bytes each, 132), a u64 at 20, a `Side`
/// spelling at 5, and the server-generated `trade_id` and its key (~35) - about
/// 390, rounded to 576. Charged separately: four id-shaped strings, one symbol
/// and one currency.
const ORDER_FILLED_MAX_BYTES: usize =
    576 + ESC * (4 * MAX_CLIENT_ID_LEN + MAX_SYMBOL_LEN + MAX_CURRENCY_LEN);

/// Widest reason-bearing lifecycle frame: client id, optional venue id, capped
/// reason, timestamp and about 100 bytes of envelope, rounded to 192.
const ORDER_REJECTION_MAX_BYTES: usize =
    192 + ESC * (2 * MAX_CLIENT_ID_LEN + crate::MAX_REASON_LEN);

pub const ORDER_EVENT_MAX_BYTES: usize = if ORDER_FILLED_MAX_BYTES > ORDER_REJECTION_MAX_BYTES {
    ORDER_FILLED_MAX_BYTES
} else {
    ORDER_REJECTION_MAX_BYTES
};

/// One `Balance` row inside `AccountState`: `currency`, `total`, `free`,
/// `locked` - three decimals at ~32 bytes plus ~60 bytes of key names,
/// punctuation and the enclosing braces, rounded to 192.
pub const BALANCE_ROW_MAX_BYTES: usize = 192 + ESC * MAX_CURRENCY_LEN;

/// One `Position` row inside `AccountState`: `symbol`, `position_id`,
/// `quantity`, `avg_px`, `mark_px`, `unrealized_pnl` - four decimals at about
/// 32 bytes plus key names and punctuation, rounded to 256.
pub const POSITION_ROW_MAX_BYTES: usize = 256 + ESC * (MAX_SYMBOL_LEN + MAX_CLIENT_ID_LEN);

pub const MARGIN_ROW_MAX_BYTES: usize = 192 + ESC * (MAX_SYMBOL_LEN + MAX_CURRENCY_LEN);

/// One `OrderStatusInfo` row inside an `OrderStatusSnapshot`:
/// `client_order_id`, `venue_order_id`, `symbol`, `side`, `order_type`,
/// `time_in_force`, `status`, `quantity`, `filled_qty`, `price`,
/// `trigger_price`, `ts_triggered`, `reduce_only`, `post_only`, `ts_accepted`,
/// `ts_last` - ~180 bytes of key names and punctuation, four decimals (132),
/// three u64s (60), four short enum spellings (~40) and two bools (10): about
/// 430, rounded to 512 on top of the charged strings.
pub const ORDER_STATUS_ROW_MAX_BYTES: usize = 512 + ESC * (3 * MAX_CLIENT_ID_LEN + MAX_SYMBOL_LEN);

/// One fill row inside a `FillSnapshot`: an `OrderFilled` plus its trade id, so
/// three client-id-shaped strings (client, venue, trade), one symbol, four
/// decimals, a u64 and two enum spellings, rounded to 320.
pub const FILL_ROW_MAX_BYTES: usize =
    384 + ESC * (4 * MAX_CLIENT_ID_LEN + MAX_SYMBOL_LEN + MAX_CURRENCY_LEN);

/// The envelope either snapshot wraps its rows in: `type`, `request_id`, the
/// row-array brackets and `ts_event`, rounded to 128 plus the echoed
/// `request_id` (capped by `validate_request_id` at `MAX_CLIENT_ID_LEN`).
pub const SNAPSHOT_ENVELOPE_MAX_BYTES: usize = 128 + ESC * MAX_CLIENT_ID_LEN;

/// What ONE executed order's LINKAGE can add to the batch it fills in.
///
/// Every sibling a filling order names produces at most one order-shaped frame:
/// an `OrderCanceled` under `Oco`, or one `OrderUpdated` (or the cancel a shrink
/// to zero becomes) under `Ouo`. Never both for one sibling, because a sibling
/// cancelled is off the book. Releasing an OTO CHILD emits nothing at all - the
/// child was accepted when it was submitted - so it costs no bytes here.
///
/// `MAX_LINKED_ORDERS` is what makes this computable in advance, and is the
/// whole reason the linkage is capped rather than open-ended.
pub const LINKAGE_MAX_BYTES: usize = MAX_LINKED_ORDERS * ORDER_EVENT_MAX_BYTES;

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
        + shape.margins * MARGIN_ROW_MAX_BYTES
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
    orders * (4 * ORDER_EVENT_MAX_BYTES + LINKAGE_MAX_BYTES)
        + account_state_max_bytes(&BookShape {
            balances: shape.balances + 2 * orders,
            positions: shape.positions + orders,
            margins: shape.margins + orders,
            ..*shape
        })
}

/// Sweep bound with venue-originated submits separated from ordinary swept
/// orders. A liquidation submit can add `OrderAccepted` to the four-frame
/// sweep shape, so counting it as an ordinary fill is not a valid bound.
#[must_use]
pub fn swept_batch_max_bytes(shape: &BookShape, swept: usize, originated: usize) -> usize {
    swept * (4 * ORDER_EVENT_MAX_BYTES + LINKAGE_MAX_BYTES)
        + originated * (5 * ORDER_EVENT_MAX_BYTES + LINKAGE_MAX_BYTES)
        + account_state_max_bytes(&BookShape {
            balances: shape.balances + 2 * (swept + originated),
            positions: shape.positions + swept + originated,
            margins: shape.margins + swept + originated,
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
        // Plus the linkage allowance: a submit that fills on arrival reaps or
        // shrinks every sibling its rule names, in the same batch, which is what
        // makes the bracket real rather than swept-later.
        ClientMessage::SubmitOrder(_) => {
            5 * ORDER_EVENT_MAX_BYTES
                + LINKAGE_MAX_BYTES
                + account_state_max_bytes(&BookShape {
                    balances: shape.balances + 2,
                    positions: shape.positions + 1,
                    margins: shape.margins + 1,
                    ..*shape
                })
        }
        // A GROUP is its members' worst cases summed, against a shape widened
        // per member for the same reason `swept_fill_max_bytes` widens per
        // order: each member can be the first fill in a new pair. Summing the
        // single-submit bound rather than deriving a tighter one is deliberate -
        // a group runs as one batch and this is an upper bound, and
        // `MAX_GROUP_ORDERS` is what keeps the sum finite.
        //
        // The linkage allowance is charged per member too, and it is not
        // double-counting: the group-closing pass applies the rule of EVERY
        // member that filled, against siblings admitted after it, so a group of
        // N filling members can emit N linkages in the one batch.
        ClientMessage::SubmitOrderGroup { orders } => {
            let members = orders.len();
            // Each member runs the ordinary submit path and takes its OWN
            // snapshot, and the group-closing linkage pass takes one more, so
            // the account is charged `members + 1` times against a shape
            // widened for the whole group. Charging one snapshot for the batch
            // - which is what a SWEEP owes, because a sweep snapshots once -
            // would under-reserve a group by a factor of its size.
            members * (5 * ORDER_EVENT_MAX_BYTES + LINKAGE_MAX_BYTES)
                + (members + 1)
                    * account_state_max_bytes(&BookShape {
                        balances: shape.balances + 2 * members,
                        positions: shape.positions + members,
                        margins: shape.margins + members,
                        ..*shape
                    })
        }
        // One order event (the cancel/update, or its rejection) plus the
        // account state that follows a book mutation - and, for a cancel, the
        // linkage allowance: cancelling a parent that never filled reaps its
        // held children in the same batch, because a child left waiting on an
        // order that is gone would rest for the life of the run. One generation
        // only, which is what the depth rule in `Engine::validate_link` buys.
        ClientMessage::CancelOrder { .. } | ClientMessage::ModifyOrder { .. } => {
            ORDER_EVENT_MAX_BYTES + LINKAGE_MAX_BYTES + account_state_max_bytes(shape)
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
            margins: 0,
            open_orders: 0,
            closed_orders: 0,
            recorded_fills: 0,
        };
        let state = AccountState {
            account_id: AccountId::parse(&"Z".repeat(MAX_ACCOUNT_ID_LEN)).unwrap(),
            balances: Vec::new(),
            positions: Vec::new(),
            margins: Vec::new(),
            ts_event: u64::MAX,
        };
        assert!(
            account_state_max_bytes(&shape) >= serde_json::to_vec(&state).unwrap().len(),
            "account snapshot bound must dominate its wire bytes"
        );
    }

    /// Finding 5's corrected derivations, RUN rather than argued. The module
    /// doc's whole claim is that every constant carries a field-by-field
    /// derivation from the struct it bounds, and both halves of
    /// `ORDER_EVENT_MAX_BYTES` had drifted from theirs - `OrderFilled` grew
    /// `position_id`, `commission_currency` and `liquidity_side` while the
    /// derivation still charged an unused `MAX_REASON_LEN` that happened to
    /// cover them. Constructing both maximal frames is what keeps a future
    /// field addition from silently voiding the reservation instead of failing
    /// here.
    ///
    /// Every string is filled with U+0001, which JSON escapes to six bytes -
    /// the worst case `JSON_ESCAPE_FACTOR` charges for. A validated id can
    /// never contain it; the bound must hold for what the TYPE can carry.
    #[test]
    fn order_event_bound_covers_both_maximal_lifecycle_frames() {
        use crate::{
            LiquiditySide, MAX_CLIENT_ID_LEN, MAX_CURRENCY_LEN, MAX_REASON_LEN, MAX_SYMBOL_LEN,
            OrderFilled, ServerMessage, Side,
        };
        use rust_decimal::Decimal;

        let worst = |len: usize| char::from(1).to_string().repeat(len);

        let filled = ServerMessage::OrderFilled(OrderFilled {
            client_order_id: worst(MAX_CLIENT_ID_LEN),
            venue_order_id: worst(MAX_CLIENT_ID_LEN),
            trade_id: worst(MAX_CLIENT_ID_LEN),
            symbol: worst(MAX_SYMBOL_LEN).into(),
            position_id: Some(worst(MAX_CLIENT_ID_LEN)),
            side: Side::Sell,
            last_qty: Decimal::MIN,
            last_px: Decimal::MIN,
            leaves_qty: Decimal::MIN,
            commission: Decimal::MIN,
            commission_currency: worst(MAX_CURRENCY_LEN),
            liquidity_side: LiquiditySide::Maker,
            ts_event: u64::MAX,
        });
        let rejected = ServerMessage::OrderRejected {
            client_order_id: worst(MAX_CLIENT_ID_LEN),
            reason: worst(MAX_REASON_LEN),
            ts_event: u64::MAX,
        };

        for (label, frame) in [("OrderFilled", &filled), ("OrderRejected", &rejected)] {
            let bytes = serde_json::to_vec(frame).unwrap().len();
            assert!(
                ORDER_EVENT_MAX_BYTES >= bytes,
                "{label} reservation {ORDER_EVENT_MAX_BYTES} must dominate {bytes} wire bytes"
            );
        }
    }

    #[test]
    fn account_id_deserialization_enforces_the_type_invariant() {
        let oversized = serde_json::to_string(&"Z".repeat(MAX_ACCOUNT_ID_LEN + 1)).unwrap();
        assert!(serde_json::from_str::<AccountId>(&oversized).is_err());
        assert!(serde_json::from_str::<AccountId>(r#""illegal value""#).is_err());
        let valid: AccountId = serde_json::from_str(r#""MOGWAI-001""#).unwrap();
        assert_eq!(valid.as_str(), "MOGWAI-001");
    }
}
