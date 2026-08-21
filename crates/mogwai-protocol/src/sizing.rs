// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Worst-case SERIALIZED-byte bounds on what one `ClientMessage` can make the
//! engine produce. It lives in the protocol crate because it is a statement
//! about the wire format: the venue reserves against it before it lets the
//! engine mutate, and the engine's own test suite checks the claim.
//!
//! A finite test matrix samples an upper bound, it cannot prove one, so every
//! constant below carries a field-by-field derivation from the struct it
//! bounds: key names and punctuation counted, each numeric at its widest
//! decimal form, each string at `JSON_ESCAPE_FACTOR` times its cap (a byte
//! serde must escape as `\uXXXX` costs six output bytes, so an ASCII-only
//! measurement would be 6x too small). The one string charged at its RAW cap
//! is the account id, and only because its TYPE forbids an escapable byte -
//! see `account_state_max_bytes`. The fixed addends are scaffolding and
//! numerics only; they are rounded upward to round figures, because
//! under-reserving voids the bound outright while over-reserving only costs a
//! connection some budget.
//!
//! THE UPWARD ROUNDING HAS A CEILING ON THE PER-STRUCT BOUNDS, AND IT IS
//! ENFORCED. Over-reservation is not free - a row constant is multiplied by a
//! row count the venue does not control and charged before the engine is
//! allowed to mutate - so this module's tests bracket each of those from BOTH
//! sides against the maximal fixture that DEFINES it: `bound >= actual` and
//! `bound < 2 * actual`, through the `brackets` helper, which carries the
//! measured slack table. Round generously WITHIN that factor; a derivation
//! that doubles its own struct is a test failure rather than a conservative
//! choice.
//!
//! THE CEILING STOPS AT THE COMPOSITIONS, DELIBERATELY. It covers the five row
//! constants, `SNAPSHOT_ENVELOPE_MAX_BYTES`, `ORDER_EVENT_MAX_BYTES` (against
//! its dominating `OrderRejected` arm) and `account_state_max_bytes`, each of
//! which has one maximal struct to be measured against. `LINKAGE_MAX_BYTES`,
//! `BOUNDARY_REFUSAL_BYTES`, `swept_fill_max_bytes` and
//! `swept_batch_max_bytes` have none: they bound the widest output a command
//! CLASS can produce, so any sample feeds one command to one book and the
//! ratio is a property of the sample. Measured over the engine's reservation
//! matrix it runs 2.2x to 249x, which is why
//! `worst_case_reservation_covers_actual_output` in `mogwai-engine` is
//! one-sided by ruling rather than by omission. `worst_case_output_bytes` is
//! bracketed on its two QUERY arms only, where the reply really does have one
//! maximal shape. A NEW COMPOSED BOUND INHERITS THAT REFUSAL; a new
//! per-struct constant owes a `brackets` call.

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
/// three fields, so it needs no bound of its own. Widest FIELD-BEARING shape,
/// which is not the same thing as the widest frame: a reason-bearing rejection
/// is wider still, and `ORDER_EVENT_MAX_BYTES` below is the max of the two -
/// measured, it resolves to the REJECTION arm, 4032 against this term's 2400,
/// because one `MAX_REASON_LEN` at the escape factor outweighs four ids, a
/// symbol and a currency. `OrderFilled` is: `type`, `client_order_id`,
/// `venue_order_id`, `trade_id`, `position_id`, `symbol`, `side`, `last_qty`,
/// `last_px`, `leaves_qty`, `commission`, `currency`, `liquidity`, `ts_event`.
/// The fixed addend covers ~190 bytes
/// of key names, quotes, colons, commas and braces, four `Decimal`s at their
/// widest serialized form (~33 bytes each, 132), a u64 at 20, a `Side`
/// spelling at 5, and the venue-generated `trade_id` and its key (~35) - about
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

/// One fill row inside a `FillSnapshot`, which is an `OrderFilled` verbatim:
/// FOUR client-id-shaped strings (client, venue, trade and the optional
/// `position_id`), one symbol, one commission currency, four decimals, a u64
/// and two enum spellings, rounded to 384. The four id-shaped strings, the
/// symbol and the currency are charged separately below; the addend is
/// scaffolding and numerics only.
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
///
/// THE ACCOUNT ID IS THE ONE STRING HERE CHARGED AT ITS RAW CAP rather than at
/// `JSON_ESCAPE_FACTOR` times it, and it is the only string in this module that
/// may be. `AccountId` is a newtype whose sole constructor is
/// `AccountId::parse` - `Deserialize` routes through it too - and that alphabet
/// is ASCII alphanumerics plus `.`, `_`, `:` and `-`, not one of which
/// `serde_json` escapes. So the type cannot carry a byte that costs more than
/// one on the wire, and a 6x factor here is not a bound, it is 320 bytes of
/// dead weight charged against every reservation that names an account - which
/// a group of nine members pays ten times over.
/// `the_account_id_alphabet_carries_nothing_json_escapes` is what holds the
/// premise: widen the alphabet and this term owes its factor back.
///
/// Every OTHER string bounded in this module is a bare `String` or `Arc<str>`
/// whose contents are capped in length and not in alphabet, so it keeps the
/// factor.
#[must_use]
pub fn account_state_max_bytes(shape: &BookShape) -> usize {
    144 + MAX_ACCOUNT_ID_LEN
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
    use crate::{
        AccountId, AccountState, Balance, ClientMessage, FillSnapshot, LiquiditySide, OrderFilled,
        OrderStatusInfo, OrderStatusSnapshot, OrderType, Position, PostedMargin, Side, TimeInForce,
        VenueMessage, WireOrderStatus,
    };
    use rust_decimal::Decimal;

    /// U+0001 fills every string these tests build, because `serde_json`
    /// escapes it to a six-byte escape sequence, which is exactly what
    /// `JSON_ESCAPE_FACTOR` charges for. No validator lets a real id carry it;
    /// the bound must hold for what the TYPE can carry, and every string
    /// bounded here except the account id is a plain `String` or `Arc<str>`
    /// capped in length and not in alphabet.
    fn worst(len: usize) -> String {
        char::from(1).to_string().repeat(len)
    }

    /// `Decimal::MIN` is the widest serialized numeric: 29 digits and a sign.
    /// Every decimal in these fixtures is at it, for the same reason every
    /// string is at its cap.
    fn maximal_balance() -> Balance {
        Balance {
            currency: worst(MAX_CURRENCY_LEN),
            total: Decimal::MIN,
            free: Decimal::MIN,
            locked: Decimal::MIN,
        }
    }

    fn maximal_position() -> Position {
        Position {
            symbol: worst(MAX_SYMBOL_LEN).into(),
            position_id: Some(worst(MAX_CLIENT_ID_LEN)),
            quantity: Decimal::MIN,
            avg_px: Decimal::MIN,
            mark_px: Decimal::MIN,
            unrealized_pnl: Decimal::MIN,
        }
    }

    fn maximal_margin() -> PostedMargin {
        PostedMargin {
            symbol: worst(MAX_SYMBOL_LEN).into(),
            currency: worst(MAX_CURRENCY_LEN),
            initial: Decimal::MIN,
            maintenance: Decimal::MIN,
        }
    }

    /// Every optional field PRESENT and every enum at its longest spelling -
    /// `TrailingStopMarket` (18), `PartiallyFilled` (15), `Gtd` - because a row
    /// bound that holds only for the short spellings is not a bound.
    fn maximal_status_row() -> OrderStatusInfo {
        OrderStatusInfo {
            client_order_id: worst(MAX_CLIENT_ID_LEN),
            venue_order_id: worst(MAX_CLIENT_ID_LEN),
            symbol: worst(MAX_SYMBOL_LEN).into(),
            position_id: Some(worst(MAX_CLIENT_ID_LEN)),
            side: Side::Sell,
            order_type: OrderType::TrailingStopMarket,
            time_in_force: TimeInForce::Gtd,
            status: WireOrderStatus::PartiallyFilled,
            quantity: Decimal::MIN,
            filled_qty: Decimal::MIN,
            price: Some(Decimal::MIN),
            trigger_price: Some(Decimal::MIN),
            ts_triggered: Some(u64::MAX),
            reduce_only: true,
            post_only: true,
            ts_accepted: u64::MAX,
            ts_last: u64::MAX,
        }
    }

    fn maximal_fill() -> OrderFilled {
        OrderFilled {
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
        }
    }

    fn empty_shape() -> BookShape {
        BookShape {
            balances: 0,
            positions: 0,
            margins: 0,
            open_orders: 0,
            closed_orders: 0,
            recorded_fills: 0,
        }
    }

    /// EVERY PER-STRUCT BOUND IN THIS MODULE IS ASSERTED FROM BOTH SIDES,
    /// against the maximal fixture that DEFINES it: it must dominate the
    /// fixture, and it must not exceed twice it. A one-sided `actual <= bound`
    /// is satisfied by a derivation that over-reserves by any factor at all,
    /// and over-reservation is not free - each of these constants is
    /// multiplied by a row count or a member count and charged against a
    /// connection's byte budget before the engine is allowed to mutate.
    ///
    /// THE COMPOSED BOUNDS ARE OUT OF SCOPE HERE AND THE MODULE HEADER SAYS
    /// WHY. `LINKAGE_MAX_BYTES`, `BOUNDARY_REFUSAL_BYTES` and the two `swept_*`
    /// helpers have no single maximal struct to be measured against, so no
    /// call below passes one to this helper - which is a ruling, not a gap.
    ///
    /// THE CEILING IS `2 * actual` AND THE FACTOR IS NOT PICKED FROM THE AIR.
    /// It is the ceiling `admission_frames_fit_their_ceiling` already uses on
    /// `ADMISSION_FRAME_MAX_BYTES` in `messages.rs`, and the measured slack of
    /// every bound here sits far inside it:
    ///
    /// | bound | maximal fixture | ratio |
    /// |---|---|---|
    /// | balance row 288 | 234 | 1.23 |
    /// | position row 832 | 785 | 1.06 |
    /// | margin row 480 | 405 | 1.19 |
    /// | order status row 1856 | 1830 | 1.01 |
    /// | fill row 2208 | 2184 | 1.01 |
    /// | snapshot envelope 512 | 474 | 1.08 |
    /// | account-state envelope 208 | 164 | 1.27 |
    /// | order event 4032 | 3545 (`OrderRejected`) | 1.14 |
    ///
    /// So the loosest is the account-state envelope at 1.27, and a ceiling
    /// tighter than 2 would still pass today - it is deliberately not taken,
    /// because these addends are ROUNDED UP to round figures by design (the
    /// module doc says so) and a small struct's rounding can legitimately
    /// approach a factor of two. What 2 forecloses is the order-of-magnitude
    /// over-reservation a one-sided assertion cannot see.
    ///
    /// THE FIXTURE MUST BE THE DOMINATING ONE. `ORDER_EVENT_MAX_BYTES` is the
    /// max of two derivations, so its ceiling is asserted against
    /// `OrderRejected` alone; the other arm is legitimately far below it and a
    /// ceiling there would be a claim about the wrong struct.
    fn brackets(label: &str, bound: usize, actual: usize) {
        assert!(
            bound >= actual,
            "{label} bound {bound} must dominate {actual} wire bytes"
        );
        assert!(
            bound < 2 * actual,
            "{label} bound {bound} over-reserves against {actual} wire bytes - a \
             reservation is charged before the engine mutates, so a derivation \
             that doubles its struct costs every connection that budget"
        );
    }

    /// The row half of the module's claim, RUN rather than argued, on the model
    /// of `order_event_bound_covers_both_maximal_lifecycle_frames`. Each of
    /// these five constants is multiplied by a row count the venue has no
    /// control over, so a derivation that drifted from its struct
    /// under-reserves by that count - and until this test existed, halving any
    /// of them left both crates green.
    ///
    /// Each row is measured ALONE, which is what these constants bound; the
    /// array commas and the envelope are the aggregate tests below.
    #[test]
    fn every_row_bound_covers_its_maximal_row() {
        let rows: [(&str, usize, Vec<u8>); 5] = [
            (
                "balance",
                BALANCE_ROW_MAX_BYTES,
                serde_json::to_vec(&maximal_balance()).unwrap(),
            ),
            (
                "position",
                POSITION_ROW_MAX_BYTES,
                serde_json::to_vec(&maximal_position()).unwrap(),
            ),
            (
                "margin",
                MARGIN_ROW_MAX_BYTES,
                serde_json::to_vec(&maximal_margin()).unwrap(),
            ),
            (
                "order status",
                ORDER_STATUS_ROW_MAX_BYTES,
                serde_json::to_vec(&maximal_status_row()).unwrap(),
            ),
            (
                "fill",
                FILL_ROW_MAX_BYTES,
                serde_json::to_vec(&maximal_fill()).unwrap(),
            ),
        ];
        for (label, bound, bytes) in rows {
            brackets(&format!("{label} row"), bound, bytes.len());
        }
    }

    /// The envelope either snapshot wraps its rows in, measured with NO rows so
    /// nothing else can pay for it. Both kinds, because the wire tag differs
    /// and it is inside the constant.
    #[test]
    fn the_snapshot_envelope_bound_covers_an_empty_reply_of_either_kind() {
        let orders = VenueMessage::OrderStatusSnapshot(OrderStatusSnapshot {
            request_id: worst(MAX_CLIENT_ID_LEN),
            orders: Vec::new(),
            ts_event: u64::MAX,
        });
        let fills = VenueMessage::FillSnapshot(FillSnapshot {
            request_id: worst(MAX_CLIENT_ID_LEN),
            fills: Vec::new(),
            ts_event: u64::MAX,
        });
        for (label, frame) in [("order status", &orders), ("fill", &fills)] {
            let bytes = serde_json::to_vec(frame).unwrap().len();
            brackets(
                &format!("{label} envelope"),
                SNAPSHOT_ENVELOPE_MAX_BYTES,
                bytes,
            );
        }
    }

    /// `account_state_max_bytes` end to end: the envelope alone at a maximal
    /// account id, and then the whole thing at seven rows of every kind, which
    /// is what catches the array commas the per-row bounds above do not charge
    /// for.
    ///
    /// BOTH FIXTURES ARE MEASURED AS `VenueMessage::AccountState`, never as a
    /// bare `AccountState`. `VenueMessage` is `#[serde(tag = "type")]`, so the
    /// frame the venue reserves for carries about 21 further bytes of tag that
    /// a bare struct does not - and this term's fixed addend is tight enough
    /// now that under-measuring by 21 would hide a halving of it.
    #[test]
    fn account_state_bound_covers_an_empty_and_a_maximal_snapshot() {
        let account_id = AccountId::parse(&"Z".repeat(MAX_ACCOUNT_ID_LEN)).unwrap();
        let empty = VenueMessage::AccountState(AccountState {
            account_id: account_id.clone(),
            balances: Vec::new(),
            positions: Vec::new(),
            margins: Vec::new(),
            ts_event: u64::MAX,
        });
        let bytes = serde_json::to_vec(&empty).unwrap().len();
        let bound = account_state_max_bytes(&empty_shape());
        brackets("empty account snapshot", bound, bytes);

        const ROWS: usize = 7;
        let full = VenueMessage::AccountState(AccountState {
            account_id,
            balances: vec![maximal_balance(); ROWS],
            positions: vec![maximal_position(); ROWS],
            margins: vec![maximal_margin(); ROWS],
            ts_event: u64::MAX,
        });
        let shape = BookShape {
            balances: ROWS,
            positions: ROWS,
            margins: ROWS,
            ..empty_shape()
        };
        let bytes = serde_json::to_vec(&full).unwrap().len();
        let bound = account_state_max_bytes(&shape);
        brackets("maximal account snapshot", bound, bytes);
    }

    /// The two query replies against the bound the venue actually reserves
    /// with - envelope plus rows, at the row counts the `BookShape` states.
    /// This is the composition `every_row_bound_covers_its_maximal_row` and the
    /// envelope test each hold one half of.
    #[test]
    fn query_reply_bounds_cover_their_maximal_snapshots() {
        const ROWS: usize = 7;
        let shape = BookShape {
            open_orders: 5,
            closed_orders: 2,
            recorded_fills: ROWS,
            ..empty_shape()
        };

        let query_orders = ClientMessage::QueryOrders {
            request_id: worst(MAX_CLIENT_ID_LEN),
            client_order_id: None,
            open_only: false,
        };
        let reply = VenueMessage::OrderStatusSnapshot(OrderStatusSnapshot {
            request_id: worst(MAX_CLIENT_ID_LEN),
            orders: vec![maximal_status_row(); ROWS],
            ts_event: u64::MAX,
        });
        let bytes = serde_json::to_vec(&reply).unwrap().len();
        let bound = worst_case_output_bytes(&query_orders, &shape);
        brackets("order status reply", bound, bytes);

        let query_fills = ClientMessage::QueryFills {
            request_id: worst(MAX_CLIENT_ID_LEN),
            client_order_id: None,
        };
        let reply = VenueMessage::FillSnapshot(FillSnapshot {
            request_id: worst(MAX_CLIENT_ID_LEN),
            fills: vec![maximal_fill(); ROWS],
            ts_event: u64::MAX,
        });
        let bytes = serde_json::to_vec(&reply).unwrap().len();
        let bound = worst_case_output_bytes(&query_fills, &shape);
        brackets("fill reply", bound, bytes);
    }

    /// THE PREMISE UNDER `account_state_max_bytes` CHARGING THE ACCOUNT ID AT
    /// ITS RAW CAP: no character `AccountId::parse` accepts costs more than one
    /// byte on the wire.
    ///
    /// The `0..=0x7f` sweep is EXHAUSTIVE, and it is the whole accepted domain
    /// because `parse` requires `is_ascii_alphanumeric` or one of four ASCII
    /// punctuation marks, so nothing above U+007F can be accepted at all.
    /// Widen that alphabet by one escapable byte and this fires - which is the
    /// signal that the dropped `JSON_ESCAPE_FACTOR` is owed back.
    ///
    /// THE TWO HALVES ARE NOT THE SAME STRENGTH, and saying so is the point of
    /// this paragraph. The ASCII half is a proof by enumeration. The
    /// "nothing above U+007F" half is a SAMPLE of six characters, which cannot
    /// prove the claim - the proof is the `parse` predicate itself, read. The
    /// sample exists so a widening that made the ASCII sweep stop being the
    /// whole domain fails something rather than nothing.
    #[test]
    fn the_account_id_alphabet_carries_nothing_json_escapes() {
        let mut accepted = 0usize;
        for byte in 0u8..=0x7f {
            let ch = char::from(byte);
            let Ok(id) = AccountId::parse(&ch.to_string()) else {
                continue;
            };
            accepted += 1;
            let bytes = serde_json::to_vec(&id).unwrap().len();
            assert_eq!(
                bytes, 3,
                "{ch:?} is accepted into an AccountId but costs {bytes} wire bytes, not 3 - \
                 the raw-cap charge in account_state_max_bytes owes its escape factor back"
            );
        }
        for ch in [
            '\u{80}',
            '\u{a0}',
            '\u{e9}',
            '\u{2028}',
            '\u{ff21}',
            '\u{1f600}',
        ] {
            assert!(
                AccountId::parse(&ch.to_string()).is_err(),
                "{ch:?} must be refused, or the ASCII sweep above is not the whole domain"
            );
        }
        assert_eq!(
            accepted,
            26 + 26 + 10 + 4,
            "the accepted alphabet is ASCII alphanumerics plus '.', '_', ':' and '-'"
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
            OrderFilled, Side, VenueMessage,
        };
        use rust_decimal::Decimal;

        let worst = |len: usize| char::from(1).to_string().repeat(len);

        let filled = VenueMessage::OrderFilled(OrderFilled {
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
        let rejected = VenueMessage::OrderRejected {
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
        // The CEILING is asserted against the dominating arm only. Measured,
        // `OrderRejected` is the wider frame - 3545 bytes against
        // `OrderFilled`'s 2205 - because `MAX_REASON_LEN` at the escape factor
        // dwarfs four ids, a symbol and a currency. `ORDER_EVENT_MAX_BYTES` is
        // therefore `ORDER_REJECTION_MAX_BYTES`, and a slack ceiling read off
        // the fill would be a claim about the struct that does NOT set this
        // constant.
        let rejected_bytes = serde_json::to_vec(&rejected).unwrap().len();
        brackets("OrderRejected event", ORDER_EVENT_MAX_BYTES, rejected_bytes);
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
