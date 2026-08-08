// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Phase 4b item 6: the TBBO stream contract refuses malformed rows instead of
//! panicking on them.
//!
//! Two INDEPENDENT defects lived here, and they are tested separately because
//! fixing either one alone leaves the other reachable:
//!
//! 1. A row with fewer fields than the header promised panicked at
//!    `parts[idx.ts_event]`, which happens BEFORE any numeric conversion. So
//!    making the conversions fallible does not reach this case at all - the
//!    width has to be checked first.
//! 2. A non-integer value in any of the six integer columns panicked inside the
//!    conversion itself.
//!
//! Both were mirrored deliberately from `analysis/mnq_fit.py`'s `parse_stream`,
//! which dies the same way, and both are fixed here BEFORE the Python retires -
//! the order the review signature is conditional on, so the reference this
//! knowingly diverges from is still runnable while the divergence lands.
//!
//! The divergence is confined to malformed input: the parity gates compare
//! output over well-formed corpora, where neither refusal is reachable.
//!
//! The fixtures are real `.csv.zst` files rather than synthesized in-process,
//! because the defect was in the streaming reader and a fixture that bypassed
//! the decoder would not have exercised it. Regenerate with
//! `zstd -f -19 <name>.csv -o <name>.csv.zst`.

use std::path::PathBuf;

use mogwai_lab::stream::ParseStream;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

/// Drains the stream and returns the first error, if any.
fn drain(name: &str) -> Option<String> {
    let mut stream = ParseStream::new(vec![fixture(name)]);
    loop {
        match stream.next() {
            None => return None,
            Some(Ok(_)) => {}
            Some(Err(e)) => return Some(e.to_string()),
        }
    }
}

#[test]
fn a_short_row_is_refused_rather_than_indexed_past() {
    let error = drain("short_row.csv.zst").expect("a truncated row must refuse");
    assert!(
        error.contains("field(s) but the header promised at least"),
        "the refusal must name the width mismatch, got: {error}"
    );
    assert!(
        error.contains("short_row.csv.zst:3"),
        "and must locate the row, got: {error}"
    );
}

#[test]
fn a_malformed_integer_is_refused_rather_than_panicking() {
    let error = drain("malformed_price.csv.zst").expect("a non-integer price must refuse");
    assert!(
        error.contains("malformed integer field price"),
        "the refusal must name the offending column, got: {error}"
    );
    assert!(
        error.contains("not-an-integer"),
        "and must quote the offending value, got: {error}"
    );
}

/// Both fixtures carry ONE well-formed row ahead of the bad one, so a refusal
/// cannot be coming from the header or from the first data row. Without this,
/// a reader that refused everything would pass both tests above.
#[test]
fn the_well_formed_row_ahead_of_each_defect_parses() {
    for name in ["short_row.csv.zst", "malformed_price.csv.zst"] {
        let mut stream = ParseStream::new(vec![fixture(name)]);
        let first = stream.next().expect("a first row").expect("which parses");
        assert_eq!(first.ts, 1_700_000_000_000_000_000);
        assert_eq!(first.price, 5_000_000_000_000);
        assert_eq!(first.side, 'B');
    }
}
