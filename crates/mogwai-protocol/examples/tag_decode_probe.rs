//! What serde's internally tagged representation costs the adapter's hot
//! decode path, and what the tag-probe decoder recovers of it.
//!
//! Three arms over the SAME representative `Trade` frame:
//!
//! - the fully general `VenueMessage` decoder, which buffers the whole object
//!   into `serde::__private::de::Content` to find the tag and then replays it;
//! - the identical payload fields as a plain untagged struct, which is the
//!   idealized single-parse floor rather than something the wire could use;
//! - `VenueMessage::from_json_str`, the landed decoder: probe the tag, then
//!   stream the payload.
//!
//! Allocations are counted by a wrapping global allocator rather than by the
//! external profiler modes, because the question is allocator CALLS per frame
//! on a path that is nanoseconds long. That also means those modes do not
//! apply to this target: it must be run plain. The registration and the
//! measured numbers are recorded in `reference/performance.md`.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use mogwai_protocol::{AggressorSide, Symbol, VenueMessage};
use rust_decimal::Decimal;
use serde::Deserialize;

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

/// `TradeTick`'s exact field set, minus the tag. Kept in sync with it by the
/// `field_parity` assertion in `main`: an arm measuring a different number of
/// fields would not be measuring the same parse.
#[derive(Deserialize)]
struct PlainTrade {
    symbol: Symbol,
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

fn main() {
    let tagged = r#"{"type":"Trade","symbol":"MNQ","price":"20123.25","size":"4","aggressor":"Buyer","ts_event":1234567890123456789}"#;
    let plain = r#"{"symbol":"MNQ","price":"20123.25","size":"4","aggressor":"Buyer","ts_event":1234567890123456789}"#;

    // Field parity between the two payload shapes, so the "plain struct" floor
    // is the same parse and not a shorter one.
    let field_parity = serde_json::from_str::<serde_json::Value>(tagged).unwrap();
    assert_eq!(
        field_parity.as_object().unwrap().len(),
        serde_json::from_str::<serde_json::Value>(plain)
            .unwrap()
            .as_object()
            .unwrap()
            .len()
            + 1,
        "the plain arm must carry every payload field and only the tag less"
    );

    let n: usize = std::env::args()
        .nth(1)
        .map_or(Ok(2_000_000), |raw| raw.parse())
        .expect("iteration count");

    ALLOCS.store(0, Ordering::Relaxed);
    let start = Instant::now();
    for _ in 0..n {
        std::hint::black_box(serde_json::from_str::<VenueMessage>(tagged).unwrap());
    }
    report("internally tagged", start.elapsed(), n);

    ALLOCS.store(0, Ordering::Relaxed);
    let start = Instant::now();
    for _ in 0..n {
        let decoded = serde_json::from_str::<PlainTrade>(plain).unwrap();
        std::hint::black_box((
            decoded.symbol,
            decoded.price,
            decoded.size,
            decoded.aggressor,
            decoded.ts_event,
        ));
    }
    report("plain struct", start.elapsed(), n);

    ALLOCS.store(0, Ordering::Relaxed);
    let start = Instant::now();
    for _ in 0..n {
        std::hint::black_box(VenueMessage::from_json_str(tagged).unwrap());
    }
    report("tag probe plus direct payload", start.elapsed(), n);

    // The same frame with an escaped tag, which a borrowed probe would refuse
    // outright. Measured too, because it is the fallback the probe's `Cow`
    // exists for and its cost should be visible rather than assumed.
    let escaped = tagged.replacen(r#""Trade""#, &format!("\"{}u0054rade\"", char::from(92)), 1);
    ALLOCS.store(0, Ordering::Relaxed);
    let start = Instant::now();
    for _ in 0..n {
        std::hint::black_box(VenueMessage::from_json_str(&escaped).unwrap());
    }
    report("tag probe, escaped tag", start.elapsed(), n);
}
