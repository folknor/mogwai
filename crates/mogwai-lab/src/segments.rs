// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The CUT half of the session-segment sampler: real session slices carved out
//! of a delivered TBBO month into a segment library the composer can loop.
//!
//! The direction this serves is `notes/segment-sampler.md` - the tape is a
//! composable session-segment sampler rather than a month imitator. This module
//! owns only the cut; the compose-and-serve half is `mogwai_data::segment`,
//! which is why the library format is a plain versioned JSON artifact rather
//! than a Rust type shared across the crate boundary (`mogwai-lab` depends on
//! `mogwai-data` and never the reverse, so a shared type would point the
//! dependency the wrong way).
//!
//! EVERYTHING IS STORED IN RETURNS SPACE. A segment carries no absolute price
//! at all: it is a sequence of log returns against its own predecessor tick,
//! plus one `open_gap_ret` measured from the last real trade BEFORE the window
//! to the first trade inside it. Absolute price level is an integration
//! constant (owner ruling, 2026-08-12), and that is exactly what lets the
//! composer butt any two segments together without a level discontinuity: the
//! incoming segment's returns compound onto whatever price the tape had
//! reached. The recorded open gap is then a real, measured reopen gap landing
//! at the seam - the owner's defect 2, which the clean generated tape does not
//! produce at all.
//!
//! Returns are stored as NANO-LOG-RETURNS: `round(ln(p / p_prev) * 1e9)` as an
//! `i64`. Integer storage keeps the artifact determinstic per binary and
//! diffable; 1e-9 of a log return is far below a tick at any level this venue
//! serves, and the bit-exactness era toward Python-era artifacts is closed, so
//! no exact-float obligation applies here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{LabError, LabResult};
use crate::session::{ScheduleFrame, format_trade_date};
use crate::stream::{Row, data_files, parse_stream};

/// The artifact format version. Bumped when the on-disk shape changes in a way
/// a previously written library cannot be read under. Distinct from
/// `TAPE_PROTOCOL_VERSION`, which identifies the GENERATION process: a library
/// is an input to that process, not the process itself.
pub const SEGMENT_LIBRARY_VERSION: u32 = 1;

const NS_PER_HOUR: u64 = 3_600_000_000_000;

/// A named session window, expressed as an offset and a length from the CME
/// trade date's own reopen instant.
///
/// Anchoring on `open_ns` rather than on a wall-clock UTC hour is what makes
/// these DST-correct without a second calendar: `ScheduleFrame::bounds` already
/// resolves the reopen civilly (17:00 America/Chicago), so a window stated as
/// "the first nine hours of the session" lands on the same civil hours either
/// side of a transition. No window here crosses a US transition instant, which
/// happens at 02:00 local on a Sunday while the venue is shut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionWindow {
    pub name: &'static str,
    /// Nanoseconds after the session reopen the window starts.
    pub offset_ns: u64,
    /// Window length in nanoseconds.
    pub length_ns: u64,
}

/// Asia: the first nine hours of the CME session, 17:00 to 02:00 exchange-local
/// (22:00 to 07:00 UTC under CDT, 23:00 to 08:00 under CST). This is the window
/// Slice 1 of the segment sampler cuts.
pub const ASIA: SessionWindow = SessionWindow {
    name: "asia",
    offset_ns: 0,
    length_ns: 9 * NS_PER_HOUR,
};

/// London: exchange-local 02:00 to 08:00, the six hours after Asia.
pub const LONDON: SessionWindow = SessionWindow {
    name: "london",
    offset_ns: 9 * NS_PER_HOUR,
    length_ns: 6 * NS_PER_HOUR,
};

/// NY morning: exchange-local 08:00 to 11:00, which is 09:00 to 12:00 New York
/// - the cash open with a half-hour lead-in, run to NY lunch.
///
/// The lead-in is the point, not padding: the direction note asks for an NY
/// tape that starts before the open so a strategy can PREPARE, and the cash
/// open at 08:30 local sits half an hour into this window rather than at its
/// edge. That also puts the owner's defect 1 - the generator smearing the
/// 09:30 New York open across its hour because the session profile is hourly -
/// inside a window where it can actually be looked at.
pub const NY_MORNING: SessionWindow = SessionWindow {
    name: "ny-morning",
    offset_ns: 15 * NS_PER_HOUR,
    length_ns: 3 * NS_PER_HOUR,
};

/// NY afternoon: exchange-local 09:30 to 15:00, which is 10:30 to 16:00 New
/// York - the second half of the cash session, ending at the cash close.
///
/// It ends at the CASH close (15:00 local), not the session close (16:00
/// local), and so stops an hour and a quarter short of the 15:15 halt. A
/// window that ran to the session close would carry the halt's fifteen-minute
/// hole and the settlement flurry, which is a different tape from the one this
/// name promises.
pub const NY_AFTERNOON: SessionWindow = SessionWindow {
    name: "ny-afternoon",
    offset_ns: 16 * NS_PER_HOUR + NS_PER_HOUR / 2,
    length_ns: 5 * NS_PER_HOUR + NS_PER_HOUR / 2,
};

/// The windows this module can cut by name.
pub const WINDOWS: [SessionWindow; 4] = [ASIA, LONDON, NY_MORNING, NY_AFTERNOON];

/// Resolves a window by its `name`.
pub fn window_by_name(name: &str) -> LabResult<SessionWindow> {
    WINDOWS
        .iter()
        .copied()
        .find(|w| w.name == name)
        .ok_or_else(|| {
            let known: Vec<&str> = WINDOWS.iter().map(|w| w.name).collect();
            LabError::refusal(format!(
                "unknown session window {name:?}; known windows are {}",
                known.join(", ")
            ))
        })
}

/// One cut session slice, in returns space.
///
/// The four tick arrays are PARALLEL and equal-length; `trade_count` is their
/// shared length, stored so a reader can refuse a truncated artifact without
/// trusting any one array.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Segment {
    /// The CME trade date whose session this slice came from.
    pub trade_date: String,
    /// The window's UTC start instant in the source month, kept for provenance
    /// only - the composer re-anchors time and never reads it.
    pub window_start_ns: u64,
    pub trade_count: usize,
    /// The log return from the last real trade BEFORE the window to this
    /// segment's first trade, in nano-log-returns. `None` when the window
    /// opened the stream and there was no prior trade to measure against.
    pub open_gap_ret: Option<i64>,
    /// Nanoseconds between the window start and the first trade, then between
    /// consecutive trades. `dt_ns[0]` is therefore the dead time at the open.
    pub dt_ns: Vec<u64>,
    /// Nano-log-return against the previous trade in this segment. `ret[0]` is
    /// always 0: the first trade IS the anchor, and its displacement from the
    /// previous session lives in `open_gap_ret`.
    pub ret: Vec<i64>,
    pub size: Vec<i64>,
    /// DBN aggressor alphabet: `B`, `A` or `N`.
    pub side: Vec<char>,
}

/// Where a library's contents came from, so a chart the owner is looking at can
/// always be traced back to the delivered bytes behind it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LibraryProvenance {
    pub symbol: String,
    pub month: String,
    pub source_dir: String,
    pub source_files: Vec<String>,
    pub cut_at: String,
}

/// A cut library: every segment of one window from one month.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SegmentLibrary {
    #[serde(rename = "_doc")]
    pub doc: String,
    pub version: u32,
    pub window: String,
    /// The instrument's price increment, as a decimal string. The composer
    /// snaps its re-anchored prices back onto this grid.
    pub tick_size: String,
    pub provenance: LibraryProvenance,
    pub segments: Vec<Segment>,
}

impl SegmentLibrary {
    /// Reads a library, refusing an artifact this build cannot interpret and
    /// one whose parallel arrays disagree.
    pub fn load(path: &Path) -> LabResult<Self> {
        let bytes = std::fs::read(path)?;
        let library: Self = serde_json::from_slice(&bytes)?;
        library.validate()?;
        Ok(library)
    }

    /// The shape contract, checked on both write and read.
    ///
    /// The array-agreement check is not ceremony: the composer indexes all four
    /// arrays with one cursor, so a short `size` array against a long `ret`
    /// array would panic deep inside a serving walk rather than at the artifact
    /// boundary where the fault actually is.
    pub fn validate(&self) -> LabResult<()> {
        if self.version != SEGMENT_LIBRARY_VERSION {
            return Err(LabError::refusal(format!(
                "segment library is version {}, this build reads {SEGMENT_LIBRARY_VERSION}",
                self.version
            )));
        }
        if self.segments.is_empty() {
            return Err(LabError::refusal(
                "segment library carries no segments; there is nothing to compose",
            ));
        }
        for segment in &self.segments {
            let n = segment.trade_count;
            if segment.dt_ns.len() != n
                || segment.ret.len() != n
                || segment.size.len() != n
                || segment.side.len() != n
            {
                return Err(LabError::refusal(format!(
                    "segment {} declares {n} trades but carries {}/{}/{}/{} \
                     dt/ret/size/side entries",
                    segment.trade_date,
                    segment.dt_ns.len(),
                    segment.ret.len(),
                    segment.size.len(),
                    segment.side.len()
                )));
            }
            if n == 0 {
                return Err(LabError::refusal(format!(
                    "segment {} is empty; an empty segment contributes no ticks and \
                     would loop forever as a zero-length draw",
                    segment.trade_date
                )));
            }
        }
        Ok(())
    }
}

/// The per-segment accumulator, alive while its window is open.
struct Cutting {
    trade_date: String,
    window_start_ns: u64,
    open_gap_ret: Option<i64>,
    dt_ns: Vec<u64>,
    ret: Vec<i64>,
    size: Vec<i64>,
    side: Vec<char>,
    last_ts: u64,
    last_price: i64,
}

impl Cutting {
    fn finish(self) -> Segment {
        Segment {
            trade_date: self.trade_date,
            window_start_ns: self.window_start_ns,
            trade_count: self.ret.len(),
            open_gap_ret: self.open_gap_ret,
            dt_ns: self.dt_ns,
            ret: self.ret,
            size: self.size,
            side: self.side,
        }
    }
}

/// `round(ln(to / from) * 1e9)`, the storage form for every return here.
///
/// Refuses a non-positive price rather than producing a NaN that would only
/// surface later as a garbage chart. TBBO carries non-positive prices on
/// administrative rows, and `classify_book` already names that condition.
fn nano_log_return(from: i64, to: i64) -> LabResult<i64> {
    if from <= 0 || to <= 0 {
        return Err(LabError::refusal(format!(
            "cannot take a log return between prices {from} and {to}; \
             a non-positive trade price has no returns-space representation"
        )));
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "TBBO prices are 1e-9 fixed point; f64 carries them exactly well past any level this venue serves"
    )]
    let ratio = (to as f64) / (from as f64);
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a log return times 1e9 is bounded by ~2e10 for any price pair a real tape produces"
    )]
    Ok((ratio.ln() * 1e9).round() as i64)
}

/// Cuts every `window` slice of `month` out of the TBBO files under
/// `source_dir`, in ONE streaming pass.
///
/// One pass matters: a delivered month is multiple GB decompressed, so the
/// windows are resolved per trade date up front and the stream is classified
/// against them as it flows, rather than re-read once per session.
/// A segment kept out of the library, and why. Reported rather than silently
/// discarded: a month that loses half its sessions to a thin-session rule is
/// something the operator has to see.
#[derive(Clone, Debug)]
pub struct DroppedSegment {
    pub trade_date: String,
    pub trade_count: usize,
    pub reason: &'static str,
}

/// The default thin-session threshold, as a fraction of the month's MEDIAN
/// segment size.
///
/// A holiday half-session is the case this exists for. CME equity index futures
/// trade a shortened Good Friday, so the 2026-04-03 `ny-morning` slice carries
/// 4,408 ticks against a 400,000-tick typical day - non-empty, so the
/// empty-segment rule keeps it, and indistinguishable from a real session to
/// everything downstream. Sampled uniformly it would then inject a
/// thirty-minute stub into an endless tape as often as a full session, which is
/// a realism defect the composer cannot detect and the owner would see on a
/// chart.
///
/// A fraction of the median rather than an absolute count, because the right
/// number differs by an order of magnitude between windows (an Asia hour and an
/// NY hour are not the same market) and by instrument. A fifth is
/// deliberately loose: it separates a half-session stub from a quiet-but-real
/// day without pretending to know where a session stops being a session.
pub const DEFAULT_MIN_TICKS_FRACTION: f64 = 0.2;

/// Cuts every `window` slice of `month` out of the TBBO files under
/// `source_dir`, in ONE streaming pass, keeping the default thin-session rule.
pub fn cut(
    source_dir: &Path,
    symbol: &str,
    month: &str,
    window: SessionWindow,
    frame: &ScheduleFrame,
) -> LabResult<SegmentLibrary> {
    cut_with(
        source_dir,
        symbol,
        month,
        window,
        frame,
        DEFAULT_MIN_TICKS_FRACTION,
    )
    .map(|(library, _)| library)
}

/// [`cut`], with the thin-session threshold exposed and the dropped segments
/// returned. `min_ticks_fraction` of 0 keeps every non-empty slice.
pub fn cut_with(
    source_dir: &Path,
    symbol: &str,
    month: &str,
    window: SessionWindow,
    frame: &ScheduleFrame,
    min_ticks_fraction: f64,
) -> LabResult<(SegmentLibrary, Vec<DroppedSegment>)> {
    if !(0.0..1.0).contains(&min_ticks_fraction) {
        return Err(LabError::refusal(format!(
            "thin-session fraction {min_ticks_fraction} is not in [0, 1); at 1 or above \
             the median session drops itself and the library empties"
        )));
    }
    let paths = data_files(source_dir)?;
    let bounds = window_bounds(month, window, frame)?;

    let mut open: Option<Cutting> = None;
    let mut done: Vec<Segment> = Vec::new();
    // The last trade seen ANYWHERE in the stream, window or not. This is what
    // makes `open_gap_ret` a real reopen gap: it is measured against the last
    // print before the window, which for Asia is the previous day's settlement
    // print, across the daily break.
    let mut last_seen_price: Option<i64> = None;

    for row in parse_stream(paths.clone()) {
        let row: Row = row?;
        let ts = u64::try_from(row.ts)
            .map_err(|_| LabError::refusal(format!("negative ts_event {} in corpus", row.ts)))?;

        // Close any window the stream has now passed. The stream is monotone in
        // ts (parse_stream refuses otherwise), so a window whose end is behind
        // this row can never receive another trade.
        if let Some(cutting) = &open
            && let Some((_, end_ns)) = bounds.get(&cutting.trade_date).copied()
            && ts >= end_ns
        {
            let Some(cutting) = open.take() else {
                unreachable!("just matched on Some");
            };
            done.push(cutting.finish());
        }

        let inside = bounds
            .iter()
            .find(|(_, (start, end))| ts >= *start && ts < *end)
            .map(|(date, (start, _))| (date.clone(), *start));

        if let Some((trade_date, window_start_ns)) = inside {
            if open.as_ref().is_none_or(|c| c.trade_date != trade_date) {
                if let Some(previous) = open.take() {
                    done.push(previous.finish());
                }
                open = Some(Cutting {
                    trade_date,
                    window_start_ns,
                    open_gap_ret: None,
                    dt_ns: Vec::new(),
                    ret: Vec::new(),
                    size: Vec::new(),
                    side: Vec::new(),
                    last_ts: window_start_ns,
                    last_price: 0,
                });
            }
            let Some(cutting) = open.as_mut() else {
                unreachable!("set immediately above");
            };
            // A non-positive or off-book print carries no returns-space
            // representation. Skipping it rather than refusing keeps one
            // administrative row from discarding a whole delivered month; the
            // count of what was skipped is not currently reported, which is a
            // gap worth closing if a cut ever looks thin.
            if row.price <= 0 {
                continue;
            }
            if cutting.ret.is_empty() {
                cutting.open_gap_ret = match last_seen_price {
                    Some(prev) if prev > 0 => Some(nano_log_return(prev, row.price)?),
                    _ => None,
                };
                cutting.ret.push(0);
            } else {
                cutting
                    .ret
                    .push(nano_log_return(cutting.last_price, row.price)?);
            }
            cutting.dt_ns.push(ts.saturating_sub(cutting.last_ts));
            cutting.size.push(row.size);
            cutting.side.push(row.side);
            cutting.last_ts = ts;
            cutting.last_price = row.price;
        }

        if row.price > 0 {
            last_seen_price = Some(row.price);
        }
    }

    if let Some(cutting) = open.take() {
        done.push(cutting.finish());
    }
    let mut dropped: Vec<DroppedSegment> = Vec::new();

    // An empty segment cannot be composed and would fail `validate` on read;
    // drop it here, at the site that knows a session simply had no prints in
    // the window, rather than failing the whole cut. Weekends and full holidays
    // leave this way and are NOT reported: a Saturday is not a session that
    // went missing, it is a day the window table never should have offered.
    done.retain(|s| s.trade_count > 0);

    // The thin-session rule, over the median of what survived. Computed AFTER
    // the empty drop so the closed days cannot drag the median down and take
    // real sessions with them.
    let threshold = {
        let mut counts: Vec<usize> = done.iter().map(|s| s.trade_count).collect();
        counts.sort_unstable();
        let median = counts.get(counts.len() / 2).copied().unwrap_or(0);
        #[expect(
            clippy::cast_precision_loss,
            reason = "a session tick count is far inside f64's exact integer range"
        )]
        let scaled = median as f64 * min_ticks_fraction;
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the fraction is in [0, 1) and the median is non-negative"
        )]
        let threshold = scaled as usize;
        threshold
    };
    done.retain(|segment| {
        if segment.trade_count >= threshold {
            return true;
        }
        dropped.push(DroppedSegment {
            trade_date: segment.trade_date.clone(),
            trade_count: segment.trade_count,
            reason: "thinner than the month's thin-session threshold; a holiday \
                     half-session composes as a stub, not as a session",
        });
        false
    });

    let library = SegmentLibrary {
        doc: format!(
            "Session-segment library for the {} window, cut from delivered TBBO. Everything is \
             in returns space: `ret` is nano-log-returns against the previous trade in the \
             segment, `open_gap_ret` the measured reopen gap from the last print before the \
             window. Absolute price level is an integration constant the composer supplies. \
             See notes/segment-sampler.md and mogwai_lab::segments.",
            window.name
        ),
        version: SEGMENT_LIBRARY_VERSION,
        window: window.name.to_string(),
        tick_size: "0.25".to_string(),
        provenance: LibraryProvenance {
            symbol: symbol.to_string(),
            month: month.to_string(),
            source_dir: source_dir.display().to_string(),
            source_files: paths
                .iter()
                .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .collect(),
            cut_at: month.to_string(),
        },
        segments: done,
    };
    library.validate()?;
    Ok((library, dropped))
}

/// `[start, end)` UTC-ns bounds of `window` for every trade date in `month`.
///
/// EVERY civil day of the month gets a window, weekends and holidays included.
/// `ScheduleFrame::bounds` is pure civil arithmetic - it resolves 17:00 local
/// for any date handed to it and knows nothing about whether the venue was
/// open - so this table is a superset of the real trade dates.
///
/// What removes the non-trading days is the CUT, not the calendar: a Saturday
/// window collects no prints, and `cut` drops every empty segment. The effect
/// is correct and needs no holiday list, but the mechanism is emptiness rather
/// than a calendar, and that is worth knowing before this is reused. A window
/// that is real but THIN would survive on the same rule, so a future caller
/// wanting "only genuine sessions" needs a minimum-trade threshold here rather
/// than the assumption that anything non-empty is a session.
fn window_bounds(
    month: &str,
    window: SessionWindow,
    frame: &ScheduleFrame,
) -> LabResult<BTreeMap<String, (u64, u64)>> {
    let (year, mon) = parse_month(month)?;
    let first = crate::session::days_from_civil(year, mon, 1);
    let next_month = if mon == 12 {
        crate::session::days_from_civil(year + 1, 1, 1)
    } else {
        crate::session::days_from_civil(year, mon + 1, 1)
    };
    let mut out = BTreeMap::new();
    for day in first..next_month {
        let label = format_trade_date(day);
        let Ok(bounds) = frame.bounds(&label) else {
            continue;
        };
        let start = bounds.open_ns + window.offset_ns;
        let end = start + window.length_ns;
        if end > bounds.close_ns {
            return Err(LabError::refusal(format!(
                "window {} runs past the {label} session close; the window table and the \
                 session calendar disagree",
                window.name
            )));
        }
        // A window overlapping the daily halt would carry its fifteen-minute
        // hole INVISIBLY: the cut would still produce a non-empty segment, the
        // composer would loop it happily, and the only symptom would be a dead
        // stretch in the middle of every looped session that no test asks
        // about. Refuse at the table instead, where the window is defined.
        if start < bounds.halt_end_ns && end > bounds.halt_start_ns {
            return Err(LabError::refusal(format!(
                "window {} overlaps the {label} trading halt; a window spanning the halt \
                 would silently carry its hole into every loop of the composed tape",
                window.name
            )));
        }
        out.insert(label, (start, end));
    }
    if out.is_empty() {
        return Err(LabError::refusal(format!(
            "month {month} contains no trade dates"
        )));
    }
    Ok(out)
}

fn parse_month(month: &str) -> LabResult<(i64, u32)> {
    let (y, m) = month
        .split_once('-')
        .ok_or_else(|| LabError::refusal(format!("month {month:?} is not YYYY-MM")))?;
    let year: i64 = y
        .parse()
        .map_err(|_| LabError::refusal(format!("month {month:?} has no four-digit year")))?;
    let mon: u32 = m
        .parse()
        .map_err(|_| LabError::refusal(format!("month {month:?} has no two-digit month")))?;
    if !(1..=12).contains(&mon) {
        return Err(LabError::refusal(format!(
            "month {month:?} is out of range"
        )));
    }
    Ok((year, mon))
}

/// The conventional corpus layout: `<root>/<symbol>v/<month>.<state>.tbbo`,
/// where `state` is whatever the delivery recorded (`full`, `manifest`,
/// `covered`, `partial`). The state is discovered rather than passed, because
/// it is a property of what the vendor delivered and not a choice the caller
/// makes.
pub fn corpus_dir(root: &Path, symbol: &str, month: &str) -> LabResult<PathBuf> {
    let instrument = root.join(format!("{}v", symbol.to_lowercase()));
    let prefix = format!("{month}.");
    let mut hits: Vec<PathBuf> = std::fs::read_dir(&instrument)
        .map_err(|e| {
            LabError::refusal(format!(
                "no corpus for {symbol} under {}: {e}",
                instrument.display()
            ))
        })?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().is_some_and(|n| {
                let n = n.to_string_lossy();
                n.starts_with(&prefix) && n.ends_with(".tbbo")
            })
        })
        .collect();
    hits.sort();
    match hits.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(LabError::refusal(format!(
            "no {month} TBBO delivery for {symbol} under {}",
            instrument.display()
        ))),
        many => Err(LabError::refusal(format!(
            "{} TBBO deliveries for {symbol} {month}; name the directory explicitly: {}",
            many.len(),
            many.iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nano_log_return_is_antisymmetric_and_zero_on_equality() {
        assert_eq!(nano_log_return(1000, 1000).unwrap(), 0);
        let up = nano_log_return(1000, 1010).unwrap();
        let down = nano_log_return(1010, 1000).unwrap();
        assert_eq!(up, -down);
        assert!(up > 0);
    }

    #[test]
    fn nano_log_return_refuses_a_nonpositive_price() {
        assert!(nano_log_return(0, 100).is_err());
        assert!(nano_log_return(100, -1).is_err());
    }

    #[test]
    fn asia_and_london_tile_the_session_without_overlap() {
        assert_eq!(ASIA.offset_ns + ASIA.length_ns, LONDON.offset_ns);
    }

    /// The four windows are stated as offsets from the 17:00 reopen, which is
    /// not how anyone thinks about them. This pins each one back to the civil
    /// exchange-local hours its name and doc claim, so a future edit to an
    /// offset cannot quietly move "the NY open" somewhere else.
    #[test]
    fn every_window_lands_on_the_exchange_local_hours_its_name_claims() {
        let local_hour = |offset_ns: u64| {
            // 17:00 local plus the offset, wrapped to a civil day.
            let minutes = 17 * 60 + offset_ns / (NS_PER_HOUR / 60);
            (minutes / 60 % 24, minutes % 60)
        };
        assert_eq!(local_hour(ASIA.offset_ns), (17, 0), "asia opens at 17:00");
        assert_eq!(
            local_hour(ASIA.offset_ns + ASIA.length_ns),
            (2, 0),
            "asia ends at 02:00"
        );
        assert_eq!(local_hour(LONDON.offset_ns), (2, 0));
        assert_eq!(local_hour(LONDON.offset_ns + LONDON.length_ns), (8, 0));
        // 08:00 local is 09:00 New York: the cash open with a lead-in.
        assert_eq!(local_hour(NY_MORNING.offset_ns), (8, 0));
        assert_eq!(
            local_hour(NY_MORNING.offset_ns + NY_MORNING.length_ns),
            (11, 0),
            "ny-morning ends at NY lunch"
        );
        assert_eq!(local_hour(NY_AFTERNOON.offset_ns), (9, 30));
        assert_eq!(
            local_hour(NY_AFTERNOON.offset_ns + NY_AFTERNOON.length_ns),
            (15, 0),
            "ny-afternoon ends at the CASH close, not the session close"
        );
    }

    #[test]
    fn every_shipped_window_clears_the_halt_and_the_session_close() {
        let authority = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../analysis/tz-america-chicago-2026c.json");
        let frame = ScheduleFrame::stage_m(&authority).expect("frozen authority");
        for window in WINDOWS {
            window_bounds("2026-04", window, &frame)
                .unwrap_or_else(|e| panic!("shipped window {} must be cuttable: {e}", window.name));
        }
    }

    /// The halt guard, checked against a window built to trip it rather than
    /// against a shipped one - a shipped window that clears the halt proves
    /// only that it clears the halt.
    #[test]
    fn a_window_spanning_the_halt_is_refused() {
        let authority = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../analysis/tz-america-chicago-2026c.json");
        let frame = ScheduleFrame::stage_m(&authority).expect("frozen authority");
        // 14:00 to 16:00 exchange-local, straight through the 15:15 halt.
        let straddles = SessionWindow {
            name: "straddles-halt",
            offset_ns: 21 * NS_PER_HOUR,
            length_ns: 2 * NS_PER_HOUR,
        };
        let err = window_bounds("2026-04", straddles, &frame)
            .expect_err("a window over the halt is refused")
            .to_string();
        assert!(err.contains("overlaps"), "{err}");
    }

    #[test]
    fn window_by_name_refuses_an_unknown_window() {
        assert_eq!(window_by_name("asia").unwrap(), ASIA);
        let err = window_by_name("tokyo").unwrap_err().to_string();
        assert!(
            err.contains("asia"),
            "the refusal lists what is known: {err}"
        );
    }

    #[test]
    fn asia_windows_land_inside_their_session_for_every_civil_day() {
        let authority = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../analysis/tz-america-chicago-2026c.json");
        let frame = ScheduleFrame::stage_m(&authority).expect("frozen authority");
        let bounds = window_bounds("2026-04", ASIA, &frame).expect("april windows");
        // Every civil day of April, weekends included: the window table is a
        // SUPERSET of the trade dates, and the cut is what drops the days with
        // no prints. Pinned here so the superset stays a deliberate property
        // rather than something a reader has to infer.
        assert_eq!(bounds.len(), 30);
        assert!(
            bounds.contains_key("2026-04-11"),
            "the table admits a Saturday; emptiness at the cut is what removes it"
        );
        for (label, (start, end)) in &bounds {
            let session = frame.bounds(label).expect("a trade date");
            assert_eq!(*start, session.open_ns, "asia starts at the reopen");
            assert!(
                *end <= session.close_ns,
                "{label} asia ends inside the session"
            );
            assert_eq!(end - start, 9 * NS_PER_HOUR);
        }
    }

    /// The writer's half of the shared fixture check. `mogwai-data` reads the
    /// same file from its own struct definitions; if the two shapes drift, one
    /// of the two tests fails rather than both staying green against a private
    /// idea of the format.
    #[test]
    fn the_conformance_fixture_round_trips_through_the_writer_types() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../analysis/segment_library_conformance.json");
        let library = SegmentLibrary::load(&path).expect("the committed fixture");
        assert_eq!(library.version, SEGMENT_LIBRARY_VERSION);
        assert_eq!(library.window, ASIA.name);
        assert_eq!(library.segments.len(), 3);
        assert_eq!(
            library.segments[0].open_gap_ret, None,
            "the first segment has no prior print to measure a gap against"
        );
        for segment in &library.segments {
            assert_eq!(segment.ret[0], 0, "a segment's first trade is its anchor");
        }
        // Re-serializing and re-reading proves the writer emits what the
        // fixture documents, not merely that it can read it.
        let text = serde_json::to_string(&library).expect("serializable");
        let again: SegmentLibrary = serde_json::from_str(&text).expect("round trip");
        again.validate().expect("a valid round trip");
        assert_eq!(again.segments.len(), library.segments.len());
    }

    #[test]
    fn the_thin_session_fraction_is_range_checked() {
        let dir = Path::new("/nonexistent");
        // The range check must fire BEFORE any file is opened, otherwise a bad
        // fraction reports a missing corpus and hides the real fault.
        for bad in [1.0, 1.5, -0.1] {
            let err = cut_with(dir, "MNQ", "2026-04", ASIA, &ScheduleFrame::JulyFixed, bad)
                .expect_err("an out-of-range fraction is refused")
                .to_string();
            assert!(err.contains("thin-session fraction"), "{err}");
        }
    }

    #[test]
    fn validate_refuses_disagreeing_parallel_arrays() {
        let library = SegmentLibrary {
            doc: String::new(),
            version: SEGMENT_LIBRARY_VERSION,
            window: "asia".into(),
            tick_size: "0.25".into(),
            provenance: LibraryProvenance {
                symbol: "MNQ".into(),
                month: "2026-04".into(),
                source_dir: String::new(),
                source_files: Vec::new(),
                cut_at: String::new(),
            },
            segments: vec![Segment {
                trade_date: "2026-04-01".into(),
                window_start_ns: 0,
                trade_count: 2,
                open_gap_ret: None,
                dt_ns: vec![0, 1],
                ret: vec![0, 5],
                size: vec![1],
                side: vec!['B', 'A'],
            }],
        };
        let err = library.validate().unwrap_err().to_string();
        assert!(err.contains("declares 2 trades"), "{err}");
    }
}
