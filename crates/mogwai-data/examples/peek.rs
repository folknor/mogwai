//! Stream the first N ticks from a Kraken pair CSV through `KrakenCsvSource`.
//!
//! Usage: `cargo run -p mogwai-data --example peek -- <path/to/PAIR.csv> [N]`

use mogwai_data::{KrakenCsvSource, TickEvent, TickSource};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: peek <PAIR.csv> [N]");
    let n: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(5);

    let mut src = KrakenCsvSource::open(&path).expect("open csv");
    println!("symbol = {}", src.symbol());
    for _ in 0..n {
        match src.next_tick() {
            Some(TickEvent::Trade(t)) => println!(
                "{} px={} sz={} {:?}",
                t.ts_event, t.price, t.size, t.aggressor
            ),
            Some(TickEvent::Quote(_)) => unreachable!("kraken dump has no quotes"),
            None => break,
        }
    }
}
