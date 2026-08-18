// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! CLI-level coverage for `mogwai presets`, the LISTING in particular.
//!
//! The bare listing shipped as a hardcoded `println!("MNQ\nMES\nBTCUSDT")` in
//! the dispatcher, beside a named lookup that already went through the server's
//! preset table. Nothing tied the two together, so adding or retiring a preset
//! moved the table while the listing kept printing the old set - and the same
//! stale count had already propagated into the README and two durable documents
//! by the time it was found. These tests drive the real binary, because the
//! defect lived in the dispatcher and a library test would have asserted
//! against the very table the dispatcher was failing to consult.

use std::process::Command;

mod common;

/// The listing is the preset table, not a copy of it.
#[test]
fn bare_presets_lists_exactly_the_shipped_preset_table() {
    let output = Command::new(common::venue_binary())
        .arg("presets")
        .output()
        .expect("running presets");
    assert!(
        output.status.success(),
        "presets exited {:?}",
        output.status
    );

    let listed: Vec<String> = String::from_utf8(output.stdout)
        .expect("the listing is utf-8")
        .lines()
        .map(str::to_owned)
        .collect();
    let expected: Vec<String> = mogwai_server::config::preset_names()
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    assert_eq!(
        listed, expected,
        "the listing must be the preset table itself, in its order"
    );
}

/// Every listed name is a name the named lookup accepts. This is the property
/// the listing exists for: a caller reads a name off it and asks for that
/// preset, so a listing naming something unfetchable is worse than no listing.
#[test]
fn every_listed_preset_is_fetchable_by_name() {
    let output = Command::new(common::venue_binary())
        .arg("presets")
        .output()
        .expect("running presets");
    // The listing's own exit status and non-emptiness are checked FIRST. The
    // body of this test is a `for` over its lines, so a failing or empty listing
    // made it vacuously green - which is precisely the "the listing went stale"
    // defect class this file exists for, passing by producing nothing to check.
    assert!(
        output.status.success(),
        "presets exited {:?}",
        output.status
    );
    let listing = String::from_utf8(output.stdout).expect("the listing is utf-8");
    let names: Vec<&str> = listing.lines().collect();
    assert!(
        !names.is_empty(),
        "the listing named no preset at all, so this test would check nothing"
    );

    for name in names {
        let fetched = Command::new(common::venue_binary())
            .arg("presets")
            .arg(name)
            .output()
            .expect("fetching a listed preset");
        assert!(
            fetched.status.success(),
            "the listing names {name}, which the named lookup refuses"
        );
        assert!(
            !fetched.stdout.is_empty(),
            "the listing names {name}, which resolves to an empty document"
        );
    }
}
