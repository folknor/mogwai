//! What an inline fixed-capacity `Symbol` would buy the adapter's decode path,
//! measured before proposing the workspace-wide edit rather than after.
//!
//! Four arms over the same representative `Trade` frame:
//!
//! - the landed `VenueMessage::from_json_str`, for scale;
//! - the payload as a plain struct with today's `Symbol = Arc<str>`;
//! - the same payload with a 32-byte inline `Copy` symbol, which is the
//!   proposal;
//! - the first thing the adapter does with a decoded trade, `convert::trade_id`
//!   replicated over the same five fields, because a saving inside the decoder
//!   is only interesting relative to what the per-tick path spends immediately
//!   after it.
//!
//! What the two payload arms do and do not share, because an undisclosed
//! asymmetry here would bias the only number this probe exists to produce:
//!
//! - They observe the same thing. Both `black_box` the whole decoded tuple,
//!   symbol value included. An earlier cut black-boxed only
//!   `symbol.as_str().len()` on the inline arm - a read of the `len: u8` field,
//!   leaving nothing observing `bytes`, so LLVM was free to elide the 32-byte
//!   `copy_from_slice` that is the inline representation's whole cost. That
//!   biased the delta toward the proposal.
//! - Neither validates. `InlineSymbol` enforces only the `MAX_SYMBOL_LEN` bound
//!   its own array needs, and today's `Arc<str>` enforces nothing, so the arms
//!   differ only in representation. The proposal's alphabet check is therefore
//!   not measured, which is conservative in the direction of the refusal: the
//!   measured delta is an upper bound on what the proposal would save. An
//!   earlier cut ran `validate_wire_symbol` on the inline arm alone, which is
//!   the same asymmetry pointing the other way.
//!
//! The `trade_id` arm is a replication and not the function itself - the adapter
//! crate cannot be depended on from `mogwai-protocol`. It omits
//! `TradeId::new_checked`, which interns the string, so its allocation count is
//! a lower bound on the real per-trade cost. That too is conservative for the
//! refusal, which is why the omission was left in place rather than modelled.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use mogwai_protocol::{AggressorSide, MAX_SYMBOL_LEN, Symbol, VenueMessage};
use rust_decimal::Decimal;
use serde::{Deserialize, Deserializer};

struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The proposal: `MAX_SYMBOL_LEN` as a property of the type.
#[derive(Clone, Copy)]
struct InlineSymbol {
    len: u8,
    bytes: [u8; MAX_SYMBOL_LEN],
}

impl InlineSymbol {
    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..usize::from(self.len)]).expect("ascii")
    }
}

impl<'de> Deserialize<'de> for InlineSymbol {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl serde::de::Visitor<'_> for V {
            type Value = InlineSymbol;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a wire symbol")
            }
            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<InlineSymbol, E> {
                // Only the bound the fixed array needs, deliberately: see the
                // module doc on why neither payload arm validates.
                if s.len() > MAX_SYMBOL_LEN {
                    return Err(E::custom("symbol exceeds MAX_SYMBOL_LEN"));
                }
                let len = u8::try_from(s.len()).map_err(E::custom)?;
                let mut bytes = [0u8; MAX_SYMBOL_LEN];
                bytes[..s.len()].copy_from_slice(s.as_bytes());
                Ok(InlineSymbol { len, bytes })
            }
        }
        d.deserialize_str(V)
    }
}

#[derive(Deserialize)]
struct ArcTrade {
    symbol: Symbol,
    price: Decimal,
    size: Decimal,
    aggressor: AggressorSide,
    ts_event: u64,
}

#[derive(Deserialize)]
struct InlineTrade {
    symbol: InlineSymbol,
    price: Decimal,
    size: Decimal,
    aggressor: AggressorSide,
    ts_event: u64,
}

fn report(label: &str, elapsed: std::time::Duration, n: usize) {
    println!(
        "{label}: ns/frame={} allocs/frame={}",
        elapsed.as_nanos() / n as u128,
        ALLOCS.load(Ordering::Relaxed) / n
    );
}

/// `mogwai_adapter::convert::trade_id`, replicated down to the 56-bit mask. The
/// adapter crate cannot be depended on from here, and the point is the order of
/// magnitude of what runs right after the decode, not the id itself. See the
/// module doc for the one thing it does not replicate.
fn trade_id(
    symbol: &str,
    ts_event: u64,
    price: Decimal,
    size: Decimal,
    a: AggressorSide,
) -> String {
    let key = format!("{symbol}-{ts_event}-{price}-{size}-{a:?}");
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{ts_event}-{:014x}", hash & 0x00ff_ffff_ffff_ffff)
}

fn main() {
    let tagged = r#"{"type":"Trade","symbol":"MNQ","price":"20123.25","size":"4","aggressor":"Buyer","ts_event":1234567890123456789}"#;
    let plain = r#"{"symbol":"MNQ","price":"20123.25","size":"4","aggressor":"Buyer","ts_event":1234567890123456789}"#;

    // Arm parity, the assertion `tag_decode_probe` was fixed to carry after a
    // measurement compared arms with mismatched fields. Two halves here: the
    // tagged frame is the plain one plus exactly the tag, and the two payload
    // structs decode the same symbol out of it, so neither arm is measuring a
    // shorter parse or a different value.
    let object_len = |raw: &str| {
        serde_json::from_str::<serde_json::Value>(raw)
            .unwrap()
            .as_object()
            .unwrap()
            .len()
    };
    assert_eq!(
        object_len(tagged),
        object_len(plain) + 1,
        "the payload arms must carry every field of the tagged frame and only the tag less"
    );
    assert_eq!(
        &*serde_json::from_str::<ArcTrade>(plain).unwrap().symbol,
        serde_json::from_str::<InlineTrade>(plain)
            .unwrap()
            .symbol
            .as_str(),
        "the two representations must decode the same symbol"
    );

    let n: usize = std::env::args()
        .nth(1)
        .map_or(Ok(2_000_000), |raw| raw.parse())
        .expect("iteration count");

    ALLOCS.store(0, Ordering::Relaxed);
    let start = Instant::now();
    for _ in 0..n {
        std::hint::black_box(VenueMessage::from_json_str(tagged).unwrap());
    }
    report("landed VenueMessage::from_json_str", start.elapsed(), n);

    ALLOCS.store(0, Ordering::Relaxed);
    let start = Instant::now();
    for _ in 0..n {
        let d = serde_json::from_str::<ArcTrade>(plain).unwrap();
        std::hint::black_box((d.symbol, d.price, d.size, d.aggressor, d.ts_event));
    }
    report("payload, Symbol = Arc<str> (today)", start.elapsed(), n);

    ALLOCS.store(0, Ordering::Relaxed);
    let start = Instant::now();
    for _ in 0..n {
        let d = serde_json::from_str::<InlineTrade>(plain).unwrap();
        // `d.symbol` whole, not a read of its `len` field: see the module doc.
        std::hint::black_box((d.symbol, d.price, d.size, d.aggressor, d.ts_event));
    }
    report("payload, inline 32-byte Symbol", start.elapsed(), n);

    let decoded = serde_json::from_str::<ArcTrade>(plain).unwrap();
    ALLOCS.store(0, Ordering::Relaxed);
    let start = Instant::now();
    for _ in 0..n {
        std::hint::black_box(trade_id(
            &decoded.symbol,
            decoded.ts_event,
            decoded.price,
            decoded.size,
            decoded.aggressor,
        ));
    }
    report("adapter convert::trade_id, one trade", start.elapsed(), n);
}
