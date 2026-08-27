// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Gate on durable prose that asserts a live fact about this crate's constants.
//!
//! Three facts have each gone stale in a durable document at least once, and
//! all three are stated in English that no compiler reads: how many order types
//! the venue serves, how many instrument classes the ledger models, and which
//! `ReadyRecord` schema version the launcher's readiness line carries. The gate
//! lives here because the three facts live here - `OrderType`,
//! `InstrumentClass` and `ReadyRecord::VERSION` are all `mogwai-protocol`.
//!
//! As with the tape-version prose gate, English is not parsed. Each fact is
//! gated on the one phrasing the documents use today, and every other mention
//! is read as historical narrative and left alone. A document recording what
//! was true at a past landing - "this reported `Canceled` until 2026-08-18",
//! "the order-type completeness ruling" - must not be rewritten when a count
//! moves, so the gate never keys on a bare number near a bare word.
//!
//! The gated phrasings, all matched against whitespace-flattened text because
//! the prose is hard-wrapped at ~72 columns and every one of these claims
//! straddles a line break:
//!
//! - `docs/oms-types.md`: `The order types the venue serves: <list>. That is
//!   every order type nautilus expresses`.
//! - `reference/architecture.md`: `<list> are served, which is every order type
//!   nautilus expresses`.
//! - `reference/architecture.md`: `The ledger models <word> instrument classes`.
//! - `reference/glossary.md`: the instrument-class entry's `<Word>: `spot``.
//!   The glossary states the end state rather than the present, so only the
//!   count it plainly asserts is gated, never its wording.
//! - `docs/cli.md`: `the version <n> `ReadyRecord``.
//!
//! `docs/oms-types.md` asserts no instrument-class count today, so there is
//! nothing to gate there; if one is written, add it to `CLASS_COUNT_SITES`.
//!
//! The two enumerations are compared as sets rather than in order: the
//! documents list `TrailingStopLimit` beside `TrailingStopMarket` where the
//! enum declares it last, and that reading order is an editorial choice the
//! gate has no business overturning. What it does hold is membership and
//! length, so a variant added to the enum and missing from a document fails,
//! and so does a name the documents carry that the wire does not.
//!
//! Every gated document is read with a panic on failure and every anchor is
//! asserted present. A gate whose phrasing has been rewritten out of the prose
//! passes forever while reporting green, which is the failure mode this file
//! exists to prevent in the documents themselves.

use std::fs;
use std::path::{Path, PathBuf};

use mogwai_protocol::{InstrumentClass, OrderType, ReadyRecord};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the crate sits two levels below the repository root")
        .to_path_buf()
}

/// Read a gated document, failing loudly when it is absent. A missing document
/// must never be a pass: the whole claim of this gate is that it held these
/// documents against the code, and a renamed or deleted file is exactly when
/// the claim stops being true.
fn gated_document(root: &Path, relative: &str) -> String {
    let path = root.join(relative);
    let text = fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "read {}: {error} - this gate holds that document against the code, \
             so a document it cannot read fails it rather than being skipped. \
             If the document moved, move the path in this test with it.",
            path.display()
        )
    });
    flatten(&text)
}

/// Collapse every whitespace run to a single space, so a claim that straddles a
/// hard-wrapped line break is still one string to match against.
fn flatten(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The variant names serde itself reports for `T`, taken from the error it
/// raises for a variant that does not exist.
///
/// This is the derivation rather than a hand-kept list, and that is the point:
/// serde's message enumerates every variant the enum declares, so a variant
/// added to the code is counted here without anybody remembering to count it.
/// The parse is deliberate about failure - no marker, or no names after it, is
/// a panic, because a silently empty list would make every check below vacuous.
fn declared_variants<T>(unknown: &str) -> Vec<String>
where
    T: serde::de::DeserializeOwned,
{
    let error = serde_json::from_str::<T>(unknown)
        .err()
        .unwrap_or_else(|| {
            panic!(
                "{unknown} deserialized successfully, so it names a real variant \
                 and cannot be used to enumerate the others - pick a spelling no \
                 variant uses"
            )
        })
        .to_string();
    const MARKER: &str = "expected one of ";
    let listed = error.split_once(MARKER).unwrap_or_else(|| {
        panic!(
            "serde reported {error:?}, which carries no {MARKER:?} - the variant \
             list is derived from that message, so this gate cannot enumerate \
             the enum any more and must be repaired rather than left green"
        )
    });
    let names: Vec<String> = listed
        .1
        .split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect();
    assert!(
        names.len() > 1,
        "serde listed {} variant names in {error:?}; the enums gated here all \
         have several, so the message format changed under this parse",
        names.len()
    );
    names
}

/// Every order type, spelled as the wire and the documents spell it.
///
/// The match carries no wildcard arm. A variant added to `OrderType` is a
/// compile error here until somebody writes its spelling down, and the round
/// trip below then carries that spelling into the document comparison, so a new
/// order type cannot reach the wire while the durable prose still enumerates
/// the old surface.
fn spelling(order_type: OrderType) -> &'static str {
    match order_type {
        OrderType::Market => "Market",
        OrderType::Limit => "Limit",
        OrderType::StopMarket => "StopMarket",
        OrderType::StopLimit => "StopLimit",
        OrderType::TrailingStopMarket => "TrailingStopMarket",
        OrderType::MarketIfTouched => "MarketIfTouched",
        OrderType::LimitIfTouched => "LimitIfTouched",
        OrderType::MarketToLimit => "MarketToLimit",
        OrderType::TrailingStopLimit => "TrailingStopLimit",
    }
}

/// The order types the durable prose must enumerate, derived from the enum.
fn served_order_types() -> Vec<String> {
    let names = declared_variants::<OrderType>("\"NoSuchOrderTypeIsDeclared\"");
    // Each derived name is fed back through serde and through the exhaustive
    // match, so the list this gate compares against is one the wire accepts and
    // one the match above has an arm for.
    names
        .iter()
        .map(|name| {
            let parsed: OrderType =
                serde_json::from_str(&format!("\"{name}\"")).unwrap_or_else(|error| {
                    panic!("serde listed variant {name:?} it rejects: {error}")
                });
            spelling(parsed).to_string()
        })
        .collect()
}

/// The enumerated order types a document states, taken from between the
/// phrasing that introduces the list and the phrasing that closes it.
///
/// Splitting on the list's own separators rather than searching for names is
/// what lets the gate see a name the documents carry and the wire does not:
/// a membership test in the other direction would pass on a document listing a
/// retired type beside the live ones.
fn enumerated(text: &str, site: &EnumerationSite) -> Vec<String> {
    let after = text.split_once(site.opens).unwrap_or_else(|| {
        panic!(
            "{}: the gated phrasing {:?} is not in the document. Either the \
             claim was rewritten - in which case reword this gate to the new \
             phrasing rather than deleting it - or the document no longer \
             states which order types are served.",
            site.path, site.opens
        )
    });
    let list = after.1.split_once(site.closes).unwrap_or_else(|| {
        panic!(
            "{}: found {:?} but not the closing phrasing {:?}, so the gate \
             cannot tell where the enumeration ends",
            site.path, site.opens, site.closes
        )
    });
    list.0
        .split(", ")
        .flat_map(|item| item.split(" and "))
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

struct EnumerationSite {
    path: &'static str,
    /// The phrasing that introduces the enumeration. Everything after it up to
    /// `closes` is read as the list.
    opens: &'static str,
    closes: &'static str,
}

/// The two documents that enumerate the order-type surface as a live claim.
const ORDER_TYPE_SITES: &[EnumerationSite] = &[
    EnumerationSite {
        path: "docs/oms-types.md",
        opens: "The order types the venue serves: ",
        closes: ". That is every order type nautilus expresses",
    },
    EnumerationSite {
        path: "reference/architecture-accounts.md",
        opens: "it is complete in fact and not only in intent: ",
        closes: " are served, which is every order type nautilus expresses",
    },
];

/// English number words, for the counts the prose spells out rather than
/// digits. The range covers every count these documents could plausibly reach
/// before somebody rewrites the sentence; a count outside it fails loudly,
/// because a gate that cannot spell the live number cannot check it.
fn number_word(count: usize) -> &'static str {
    const WORDS: [&str; 13] = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
        "eleven", "twelve",
    ];
    WORDS.get(count).copied().unwrap_or_else(|| {
        panic!(
            "no number word for {count} - extend the table, because the prose \
             spells these counts out"
        )
    })
}

struct CountSite {
    path: &'static str,
    /// The text immediately before the count in the gated sentence.
    before: &'static str,
    /// The text immediately after it.
    after: &'static str,
    /// Whether the sentence capitalizes the number word.
    capitalized: bool,
}

/// The documents that state the instrument-class count as a live claim.
const CLASS_COUNT_SITES: &[CountSite] = &[
    CountSite {
        path: "reference/architecture-accounts.md",
        before: "The ledger models ",
        after: " instrument classes, split by settlement shape",
        capitalized: false,
    },
    CountSite {
        path: "reference/glossary.md",
        before: "the settlement shape an instrument takes, which is what decides how holding it \
                 moves the ledger. ",
        after: ": `spot`",
        capitalized: true,
    },
];

/// Read the number word a count site states, failing when the sentence itself
/// is gone rather than when only the number moved. The two failures need
/// different repairs, so they are reported apart.
fn stated_word(text: &str, site: &CountSite) -> String {
    let after = text.split_once(site.before).unwrap_or_else(|| {
        panic!(
            "{}: the gated phrasing {:?} is not in the document. Either the \
             claim was reworded - reword this gate with it - or the document \
             stopped stating the count.",
            site.path, site.before
        )
    });
    let word = after.1.split_once(site.after).unwrap_or_else(|| {
        panic!(
            "{}: found {:?} but not the following {:?}, so the gate cannot tell \
             which word is the count",
            site.path, site.before, site.after
        )
    });
    word.0.to_string()
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[test]
fn durable_prose_enumerates_the_order_types_the_wire_declares() {
    let root = repo_root();
    let served = served_order_types();
    assert!(
        served.len() > 1,
        "the order-type derivation produced {} names, so nothing below is being \
         checked",
        served.len()
    );

    let mut sorted_served = served.clone();
    sorted_served.sort();

    for site in ORDER_TYPE_SITES {
        let text = gated_document(&root, site.path);
        let mut listed = enumerated(&text, site);
        let listed_len = listed.len();
        listed.sort();
        assert_eq!(
            listed,
            sorted_served,
            "{}: the enumerated order types are stale. The document lists {} \
             type(s) and the wire declares {} - update the sentence introduced \
             by {:?} to enumerate exactly: {}",
            site.path,
            listed_len,
            served.len(),
            site.opens,
            served.join(", ")
        );
    }
}

#[test]
fn durable_prose_states_the_instrument_class_count_the_wire_declares() {
    let root = repo_root();
    let classes = declared_variants::<InstrumentClass>("{\"class\":\"no_such_instrument_class\"}");
    let expected = number_word(classes.len());

    for site in CLASS_COUNT_SITES {
        let text = gated_document(&root, site.path);
        let stated = stated_word(&text, site);
        let want = if site.capitalized {
            capitalize(expected)
        } else {
            expected.to_string()
        };
        assert_eq!(
            stated,
            want,
            "{}: the instrument-class count is stale. The document says {:?} \
             where the wire declares {} classes ({}) - write {:?} in the \
             sentence between {:?} and {:?}, and check the list beside it.",
            site.path,
            stated,
            classes.len(),
            classes.join(", "),
            want,
            site.before,
            site.after
        );
    }
}

#[test]
fn the_cli_guide_names_the_live_ready_record_version() {
    let root = repo_root();
    let text = gated_document(&root, "docs/cli.md");
    // The live claim is the readiness line's own description. Mentions of an
    // older version elsewhere are records of what a past launcher wrote and are
    // deliberately not matched by this phrasing.
    let site = CountSite {
        path: "docs/cli.md",
        before: "the version ",
        after: " `ReadyRecord`",
        capitalized: false,
    };
    let stated = stated_word(&text, &site);
    assert_eq!(
        stated,
        ReadyRecord::VERSION.to_string(),
        "docs/cli.md claims the readiness line carries the version {stated} \
         `ReadyRecord`, but `ReadyRecord::VERSION` is {} - update the sentence \
         to say `the version {} `ReadyRecord``.",
        ReadyRecord::VERSION,
        ReadyRecord::VERSION
    );
}
