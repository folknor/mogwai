// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The OBSERVED front-end of the unified block engine: a port of
//! `analysis/mnq_fit.py`'s `measure12a_observe`. One chronological pass over
//! the TBBO stream, ONE session retained at a time.
//!
//! Parent inference here is the frozen observed rule: a parent is a
//! contiguous run of rows sharing `(ts, side)`; an unsided (`N`) row never
//! joins a parent AND terminates the open one; a row outside the usable
//! session set terminates the open parent too. The parent's segment is the
//! one its FIRST child fell in, and its book/quote come from that same first
//! child's row - the parent is opened from the row that starts it and never
//! re-reads the book.

use crate::error::{LabError, LabResult};
use crate::measure12a::{Scope, SessionAcc};
use crate::session::{MinuteFieldsCache, session_segment_at};
use crate::stream::Row;
use crate::subcontract::UTC_OFFSET_MINUTES;

/// One open parent: the `(ts, side)` key, the segment index of its first
/// child, and that child's book.
struct OpenParent {
    ts: i64,
    side: char,
    segment_index: u8,
    first_ts: u64,
    bid_px: i64,
    ask_px: i64,
    book_normal: bool,
}

/// `measure12a_observe`: the per-session sufficient records of the observed
/// half. `usable` is the preflight's usable-session list; the returned record
/// order must equal `sorted(usable)` or the pass refuses (the Python's own
/// terminal check).
pub fn observe<I>(rows: I, usable: &[String]) -> LabResult<Vec<serde_json::Value>>
where
    I: IntoIterator<Item = LabResult<Row>>,
{
    observe_with_count_windows(rows, usable, crate::subcontract::COUNT_WINDOWS_S)
}

/// The protocol-12a observed path, parameterized only at its Block 2 window
/// set so extensions reuse the frozen parent inference and window scheduler.
pub fn observe_with_count_windows<I>(
    rows: I,
    usable: &[String],
    count_windows_s: &'static [i64],
) -> LabResult<Vec<serde_json::Value>>
where
    I: IntoIterator<Item = LabResult<Row>>,
{
    let usable_set: std::collections::BTreeSet<&str> = usable.iter().map(String::as_str).collect();
    let mut minutes = MinuteFieldsCache::new();
    let mut records: Vec<serde_json::Value> = Vec::new();
    let mut state: Option<SessionAcc> = None;
    let mut current: Option<OpenParent> = None;

    for row in rows {
        let row = row?;
        let ts = u64::try_from(row.ts)
            .map_err(|_| LabError::refusal(format!("negative ts_event {}", row.ts)))?;
        let (session, segment, _hour) = minutes.minute_fields(ts);
        let in_usable = session.as_deref().is_some_and(|s| usable_set.contains(s));
        if !in_usable {
            close_parent(&mut state, current.take())?;
            continue;
        }
        let session = session.expect("in_usable implies a session");
        let segment = segment.expect("a resolved session always resolves a segment");
        let segment_index = u8::from(segment == "post_halt");
        if state.as_ref().is_none_or(|s| s.date != session) {
            close_parent(&mut state, current.take())?;
            if let Some(done) = state.take() {
                records.push(done.close(Scope::Observed)?);
            }
            let seg = session_segment_at(ts, UTC_OFFSET_MINUTES)
                .ok_or_else(|| LabError::refusal(format!("row at {ts} maps to no open segment")))?;
            state = Some(SessionAcc::new_with_count_windows(
                session,
                &seg,
                UTC_OFFSET_MINUTES,
                count_windows_s,
            ));
        }
        // The trade range covers ALL structurally valid prints - unsided rows
        // and invalid books included - by the print's own timestamp.
        if let Some(acc) = state.as_mut() {
            acc.push_print(ts, row.price);
        }
        if row.side == 'N' {
            close_parent(&mut state, current.take())?;
            continue;
        }
        match &mut current {
            Some(open) if open.ts == row.ts && open.side == row.side => {}
            _ => {
                close_parent(&mut state, current.take())?;
                current = Some(OpenParent {
                    ts: row.ts,
                    side: row.side,
                    segment_index,
                    first_ts: ts,
                    bid_px: row.bid_px,
                    ask_px: row.ask_px,
                    book_normal: row.book == "normal",
                });
            }
        }
    }
    close_parent(&mut state, current.take())?;
    if let Some(done) = state.take() {
        records.push(done.close(Scope::Observed)?);
    }
    let got: Vec<&str> = records
        .iter()
        .map(|r| r["session_date"].as_str().unwrap_or_default())
        .collect();
    let mut want: Vec<&str> = usable.iter().map(String::as_str).collect();
    want.sort_unstable();
    if got != want {
        return Err(LabError::refusal(format!(
            "measure12a session records do not match the usable set: {got:?} vs {want:?}"
        )));
    }
    Ok(records)
}

/// The frozen observed pass with the ordered one-second sufficient evidence
/// extracted before each session accumulator is reduced.
pub fn observe_ordered<I>(
    rows: I,
    usable: &[String],
) -> LabResult<(Vec<serde_json::Value>, Vec<crate::measure12a::OrderedCount>)>
where
    I: IntoIterator<Item = LabResult<Row>>,
{
    observe_impl(rows, usable, Some(Vec::new()))
}

fn observe_impl<I>(
    rows: I,
    usable: &[String],
    mut ordered: Option<Vec<crate::measure12a::OrderedCount>>,
) -> LabResult<(Vec<serde_json::Value>, Vec<crate::measure12a::OrderedCount>)>
where
    I: IntoIterator<Item = LabResult<Row>>,
{
    let usable_set: std::collections::BTreeSet<&str> = usable.iter().map(String::as_str).collect();
    let mut minutes = MinuteFieldsCache::new();
    let mut records = Vec::new();
    let mut state: Option<SessionAcc> = None;
    let mut current: Option<OpenParent> = None;
    for row in rows {
        let row = row?;
        let ts = u64::try_from(row.ts)
            .map_err(|_| LabError::refusal(format!("negative ts_event {}", row.ts)))?;
        let (session, segment, _) = minutes.minute_fields(ts);
        let in_usable = session.as_deref().is_some_and(|s| usable_set.contains(s));
        if !in_usable {
            close_parent(&mut state, current.take())?;
            continue;
        }
        let session = session.expect("usable session");
        let segment = segment.expect("resolved segment");
        let segment_index = u8::from(segment == "post_halt");
        if state.as_ref().is_none_or(|s| s.date != session) {
            close_parent(&mut state, current.take())?;
            if let Some(mut done) = state.take() {
                ordered
                    .as_mut()
                    .expect("ordered enabled")
                    .extend(done.ordered_counts()?);
                records.push(done.close(Scope::Observed)?);
            }
            let seg = session_segment_at(ts, UTC_OFFSET_MINUTES)
                .ok_or_else(|| LabError::refusal(format!("row at {ts} maps to no open segment")))?;
            state = Some(SessionAcc::new(session, &seg, UTC_OFFSET_MINUTES));
        }
        if let Some(acc) = state.as_mut() {
            acc.push_print(ts, row.price);
        }
        if row.side == 'N' {
            close_parent(&mut state, current.take())?;
            continue;
        }
        match &mut current {
            Some(open) if open.ts == row.ts && open.side == row.side => {}
            _ => {
                close_parent(&mut state, current.take())?;
                current = Some(OpenParent {
                    ts: row.ts,
                    side: row.side,
                    segment_index,
                    first_ts: ts,
                    bid_px: row.bid_px,
                    ask_px: row.ask_px,
                    book_normal: row.book == "normal",
                });
            }
        }
    }
    close_parent(&mut state, current.take())?;
    if let Some(mut done) = state.take() {
        ordered
            .as_mut()
            .expect("ordered enabled")
            .extend(done.ordered_counts()?);
        records.push(done.close(Scope::Observed)?);
    }
    let got = records
        .iter()
        .map(|r| r["session_date"].as_str().unwrap_or_default())
        .collect::<Vec<_>>();
    let mut want = usable.iter().map(String::as_str).collect::<Vec<_>>();
    want.sort_unstable();
    if got != want {
        return Err(LabError::refusal(format!(
            "measure12a session records do not match the usable set: {got:?} vs {want:?}"
        )));
    }
    Ok((records, ordered.unwrap_or_default()))
}

fn close_parent(state: &mut Option<SessionAcc>, parent: Option<OpenParent>) -> LabResult<()> {
    let (Some(acc), Some(parent)) = (state.as_mut(), parent) else {
        return Ok(());
    };
    acc.push_parent(
        parent.segment_index,
        parent.first_ts,
        parent.bid_px,
        parent.ask_px,
        parent.book_normal,
    )
}
