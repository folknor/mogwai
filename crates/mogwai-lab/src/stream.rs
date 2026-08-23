// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The TBBO `.csv.zst` stream contract (`analysis/mnq_fit.py`
//! `iter_csv_zst`/`column_indices`/`Row`/`classify_book`/`parse_stream`/
//! `group_parents_batch`). Ordering, off-grid prices, the seam
//! duplicate-detection window, CRLF handling and the header contract are all
//! load-bearing per the phase-1 brief; every `Refusal` branch below has a
//! matching one in the Python source.

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use crate::error::{LabError, LabResult};
use crate::subcontract::TICK_UNITS;

const REQUIRED_COLUMNS: [&str; 10] = [
    "ts_event",
    "instrument_id",
    "action",
    "side",
    "price",
    "size",
    "bid_px_00",
    "ask_px_00",
    "bid_sz_00",
    "ask_sz_00",
];

/// Streams decompressed text lines from a `.csv.zst` file, header included.
/// A trailing `\r` is stripped so CRLF data parses identically to LF (the
/// Python reference's `iter_csv_zst` docstring: "the seam comparison sees
/// the same bytes either way").
struct ZstLines<R> {
    reader: R,
    pending: Vec<u8>,
    eof: bool,
}

impl<R: Read> ZstLines<R> {
    fn new(reader: R) -> Self {
        Self {
            reader,
            pending: Vec::new(),
            eof: false,
        }
    }
}

impl<R: Read> Iterator for ZstLines<R> {
    type Item = LabResult<String>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(pos) = self.pending.iter().position(|&b| b == b'\n') {
                let mut line: Vec<u8> = self.pending.drain(..=pos).collect();
                line.pop(); // the '\n'
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Some(
                    String::from_utf8(line)
                        .map_err(|e| LabError::refusal(format!("non-utf8 row bytes: {e}"))),
                );
            }
            if self.eof {
                if self.pending.is_empty() {
                    return None;
                }
                let mut line = std::mem::take(&mut self.pending);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Some(
                    String::from_utf8(line)
                        .map_err(|e| LabError::refusal(format!("non-utf8 row bytes: {e}"))),
                );
            }
            let mut chunk = [0u8; 1 << 20];
            match self.reader.read(&mut chunk) {
                Ok(0) => self.eof = true,
                Ok(n) => self.pending.extend_from_slice(&chunk[..n]),
                Err(e) => return Some(Err(LabError::from(e))),
            }
        }
    }
}

fn iter_csv_zst(path: &Path) -> LabResult<impl Iterator<Item = LabResult<String>> + use<>> {
    let file = File::open(path)?;
    let decoder = zrip::FrameDecoder::new(BufReader::new(file));
    Ok(ZstLines::new(decoder))
}

#[derive(Clone, Copy, Debug)]
struct ColumnIndices {
    ts_event: usize,
    instrument_id: usize,
    action: usize,
    side: usize,
    price: usize,
    size: usize,
    bid_px_00: usize,
    ask_px_00: usize,
    bid_sz_00: usize,
    ask_sz_00: usize,
    /// One past the largest index any of the above will dereference, so a
    /// short row can be refused once, before the first access, rather than
    /// guarded at each of the ten sites individually.
    required_width: usize,
}

fn column_indices(header_line: &str) -> LabResult<ColumnIndices> {
    let names: Vec<&str> = header_line.trim().split(',').collect();
    let find = |want: &str| names.iter().position(|n| *n == want);
    let missing: Vec<&str> = REQUIRED_COLUMNS
        .iter()
        .filter(|c| find(c).is_none())
        .copied()
        .collect();
    if !missing.is_empty() {
        return Err(LabError::refusal(format!(
            "header is missing required column(s) {}; got: {}",
            missing.join(", "),
            header_line.trim()
        )));
    }
    let indices = ColumnIndices {
        ts_event: find("ts_event").unwrap(),
        instrument_id: find("instrument_id").unwrap(),
        action: find("action").unwrap(),
        side: find("side").unwrap(),
        price: find("price").unwrap(),
        size: find("size").unwrap(),
        bid_px_00: find("bid_px_00").unwrap(),
        ask_px_00: find("ask_px_00").unwrap(),
        bid_sz_00: find("bid_sz_00").unwrap(),
        ask_sz_00: find("ask_sz_00").unwrap(),
        required_width: 0,
    };
    let required_width = 1 + [
        indices.ts_event,
        indices.instrument_id,
        indices.action,
        indices.side,
        indices.price,
        indices.size,
        indices.bid_px_00,
        indices.ask_px_00,
        indices.bid_sz_00,
        indices.ask_sz_00,
    ]
    .into_iter()
    .max()
    .expect("ten fixed columns");
    Ok(ColumnIndices {
        required_width,
        ..indices
    })
}

/// Lists the `.csv.zst` files under `directory`, sorted by name (the
/// stream's file order, load-bearing for the seam check and ordering
/// contract). Refuses if none exist.
pub fn data_files(directory: &Path) -> LabResult<Vec<PathBuf>> {
    let mut names: Vec<String> = std::fs::read_dir(directory)?
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.ends_with(".csv.zst"))
        .collect();
    if names.is_empty() {
        return Err(LabError::refusal(format!(
            "no .csv.zst files under {}",
            directory.display()
        )));
    }
    names.sort();
    Ok(names.into_iter().map(|n| directory.join(n)).collect())
}

#[derive(Clone, Debug)]
pub struct Row {
    pub ts: i64,
    pub instrument_id: String,
    pub side: char,
    pub price: i64,
    pub size: i64,
    pub bid_px: i64,
    pub ask_px: i64,
    pub bid_sz: i64,
    pub ask_sz: i64,
    pub book: &'static str,
}

/// `classify_book`: `nonpositive` if either side is `<= 0`, `crossed` if
/// `ask < bid`, `locked` if `ask == bid`, else `normal`.
pub fn classify_book(bid_px: i64, ask_px: i64) -> &'static str {
    if bid_px <= 0 || ask_px <= 0 {
        "nonpositive"
    } else if ask_px < bid_px {
        "crossed"
    } else if ask_px == bid_px {
        "locked"
    } else {
        "normal"
    }
}

/// Reads one integer column, refusing rather than panicking on a malformed
/// value.
///
/// This used to `panic!`, mirroring `mnq_fit.py`'s `parse_stream`, which
/// carries no named refusal for these conversions either and dies on a
/// non-integer field. The mirror was deliberate while both implementations had
/// to agree, and it is fixed here as phase 4b item 6 - before the Python
/// retires, so its behaviour is still available as the reference this diverges
/// from knowingly.
///
/// The mismatch is safe by construction for the parity gates: they compare
/// output over well-formed corpora, where no conversion fails and nothing is
/// reached. What changes is only the behaviour on input the Python crashes on,
/// which is the class the refusal contract in `reference/architecture.md`
/// exists to cover.
fn parse_field_i64(
    parts: &[&str],
    idx: usize,
    path: &Path,
    line_no: usize,
    field: &str,
) -> LabResult<i64> {
    let raw = parts.get(idx).ok_or_else(|| {
        LabError::refusal(format!(
            "{}:{line_no}: row ends before column {field}",
            path.display()
        ))
    })?;
    raw.parse::<i64>().map_err(|e| {
        LabError::refusal(format!(
            "{}:{line_no}: malformed integer field {field} {raw:?}: {e}",
            path.display()
        ))
    })
}

/// The streaming pass over the ordered files as one stream
/// (`analysis/mnq_fit.py` `parse_stream`): 19-digit ns timestamps, monotone
/// ordering across the file boundary, no duplicate row at the seam, the
/// per-price grid, strict B/A/N sides, action `T` on every row.
pub struct ParseStream {
    paths: Vec<PathBuf>,
    file_idx: usize,
    lines: Option<Box<dyn Iterator<Item = LabResult<String>>>>,
    idx: Option<ColumnIndices>,
    line_no: usize,
    prev_ts: Option<i64>,
    seam_ts: Option<i64>,
    seam_lines: HashSet<String>,
    tail_ts: Option<i64>,
    tail_lines: HashSet<String>,
    in_seam: bool,
    finished: bool,
}

impl ParseStream {
    pub fn new(paths: Vec<PathBuf>) -> Self {
        Self {
            paths,
            file_idx: 0,
            lines: None,
            idx: None,
            line_no: 1,
            prev_ts: None,
            seam_ts: None,
            seam_lines: HashSet::new(),
            tail_ts: None,
            tail_lines: HashSet::new(),
            in_seam: false,
            finished: false,
        }
    }

    fn open_next_file(&mut self) -> Option<LabResult<()>> {
        if self.file_idx >= self.paths.len() {
            return None;
        }
        let path = self.paths[self.file_idx].clone();
        let mut lines: Box<dyn Iterator<Item = LabResult<String>>> = match iter_csv_zst(&path) {
            Ok(l) => Box::new(l),
            Err(e) => return Some(Err(e)),
        };
        let header = match lines.next() {
            Some(Ok(h)) => h,
            Some(Err(e)) => return Some(Err(e)),
            None => {
                return Some(Err(LabError::refusal(format!(
                    "{} is empty",
                    path.display()
                ))));
            }
        };
        let idx = match column_indices(&header) {
            Ok(i) => i,
            Err(e) => return Some(Err(e)),
        };
        self.lines = Some(lines);
        self.idx = Some(idx);
        self.line_no = 1;
        self.in_seam = self.seam_ts.is_some();
        self.tail_ts = None;
        self.tail_lines = HashSet::new();
        Some(Ok(()))
    }
}

impl Iterator for ParseStream {
    type Item = LabResult<Row>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        loop {
            if self.lines.is_none() {
                match self.open_next_file() {
                    None => {
                        self.finished = true;
                        return None;
                    }
                    Some(Err(e)) => {
                        self.finished = true;
                        return Some(Err(e));
                    }
                    Some(Ok(())) => {}
                }
            }
            let path = self.paths[self.file_idx].clone();
            let Some(lines) = self.lines.as_mut() else {
                unreachable!("just opened above");
            };
            let next_line = lines.next();
            match next_line {
                None => {
                    if self.tail_ts.is_none() {
                        self.finished = true;
                        return Some(Err(LabError::refusal(format!(
                            "{} carries a header but no data rows; an empty intermediate file \
                             would also silently reset the seam check",
                            path.display()
                        ))));
                    }
                    self.seam_ts = self.tail_ts;
                    self.seam_lines = std::mem::take(&mut self.tail_lines);
                    self.file_idx += 1;
                    self.lines = None;
                }
                Some(Err(e)) => {
                    self.finished = true;
                    return Some(Err(e));
                }
                Some(Ok(line)) => {
                    self.line_no += 1;
                    if line.trim().is_empty() {
                        continue;
                    }
                    let Some(idx) = self.idx else {
                        unreachable!("column indices set with the current file");
                    };
                    let parts: Vec<&str> = line.split(',').collect();
                    // Width first, before any indexed access. Every dereference
                    // below uses a position derived from the header, so a row
                    // with fewer fields than the header promised used to panic
                    // on the very first one - `parts[idx.ts_event]` - which is
                    // earlier than any conversion and therefore not reachable by
                    // making the conversions fallible. Phase 4b item 6.
                    if parts.len() < idx.required_width {
                        self.finished = true;
                        return Some(Err(LabError::refusal(format!(
                            "{}:{}: row carries {} field(s) but the header promised at least {}; \
                             truncated or mis-delimited input is refused rather than indexed past",
                            path.display(),
                            self.line_no,
                            parts.len(),
                            idx.required_width
                        ))));
                    }
                    let raw_ts = parts[idx.ts_event];
                    if raw_ts.len() != 19 || !raw_ts.bytes().all(|b| b.is_ascii_digit()) {
                        self.finished = true;
                        return Some(Err(LabError::refusal(format!(
                            "{}:{}: ts_event {:?} is not a 19-digit nanosecond epoch",
                            path.display(),
                            self.line_no,
                            raw_ts
                        ))));
                    }
                    let Ok(ts) = raw_ts.parse::<i64>() else {
                        unreachable!("checked all-digit above");
                    };
                    if let Some(prev) = self.prev_ts
                        && ts < prev
                    {
                        self.finished = true;
                        return Some(Err(LabError::refusal(format!(
                            "{}:{}: ordering regression ({ts} after {prev}) - zero are \
                             tolerated",
                            path.display(),
                            self.line_no
                        ))));
                    }
                    if self.in_seam {
                        if Some(ts) != self.seam_ts {
                            self.in_seam = false;
                        } else if self.seam_lines.contains(&line) {
                            self.finished = true;
                            return Some(Err(LabError::refusal(format!(
                                "{}:{}: duplicates a row of the previous file's final timestamp \
                                 at the boundary; the files overlap",
                                path.display(),
                                self.line_no
                            ))));
                        }
                    }
                    let action = parts[idx.action];
                    if action != "T" {
                        self.finished = true;
                        return Some(Err(LabError::refusal(format!(
                            "{}:{}: action {:?} is not T; the tbbo schema carries one trade per \
                             row",
                            path.display(),
                            self.line_no,
                            action
                        ))));
                    }
                    let side = parts[idx.side];
                    if !matches!(side, "B" | "A" | "N") {
                        self.finished = true;
                        return Some(Err(LabError::refusal(format!(
                            "{}:{}: side {:?} outside the DBN alphabet B/A/N",
                            path.display(),
                            self.line_no,
                            side
                        ))));
                    }
                    // The refusal has to become the iterator's error item rather
                    // than propagate with `?`, since this is `Iterator::next`.
                    // One macro rather than six identical matches, so the
                    // conversion sites stay as readable as they were when they
                    // panicked.
                    macro_rules! field {
                        ($index:expr, $name:literal) => {
                            match parse_field_i64(&parts, $index, &path, self.line_no, $name) {
                                Ok(value) => value,
                                Err(e) => {
                                    self.finished = true;
                                    return Some(Err(e));
                                }
                            }
                        };
                    }
                    let price = field!(idx.price, "price");
                    let bid_px = field!(idx.bid_px_00, "bid_px_00");
                    let ask_px = field!(idx.ask_px_00, "ask_px_00");
                    if price > 0 && price % TICK_UNITS != 0 {
                        self.finished = true;
                        return Some(Err(LabError::refusal(format!(
                            "{}:{}: price {price} is off the 0.25 grid",
                            path.display(),
                            self.line_no
                        ))));
                    }
                    if bid_px > 0 && bid_px % TICK_UNITS != 0 {
                        self.finished = true;
                        return Some(Err(LabError::refusal(format!(
                            "{}:{}: bid_px_00 {bid_px} is off the 0.25 grid",
                            path.display(),
                            self.line_no
                        ))));
                    }
                    if ask_px > 0 && ask_px % TICK_UNITS != 0 {
                        self.finished = true;
                        return Some(Err(LabError::refusal(format!(
                            "{}:{}: ask_px_00 {ask_px} is off the 0.25 grid",
                            path.display(),
                            self.line_no
                        ))));
                    }
                    let size = field!(idx.size, "size");
                    let bid_sz = field!(idx.bid_sz_00, "bid_sz_00");
                    let ask_sz = field!(idx.ask_sz_00, "ask_sz_00");
                    let row = Row {
                        ts,
                        instrument_id: parts[idx.instrument_id].to_string(),
                        side: {
                            let Some(c) = side.chars().next() else {
                                unreachable!("checked non-empty above")
                            };
                            c
                        },
                        price,
                        size,
                        bid_px,
                        ask_px,
                        bid_sz,
                        ask_sz,
                        book: classify_book(bid_px, ask_px),
                    };
                    self.prev_ts = Some(ts);
                    if Some(ts) != self.tail_ts {
                        self.tail_ts = Some(ts);
                        self.tail_lines = HashSet::from([line]);
                    } else {
                        self.tail_lines.insert(line);
                    }
                    return Some(Ok(row));
                }
            }
        }
    }
}

pub fn parse_stream(paths: Vec<PathBuf>) -> ParseStream {
    ParseStream::new(paths)
}

/// `group_parents_batch`: the independent second implementation of the
/// frozen grouping rule, used to police the streaming pass over a
/// materialized slice - contiguous `(ts, side)` runs, splitting wherever the
/// key changes; an unsided (`N`) row never enters a group and terminates the
/// open one. Returns `[start, end)` index ranges into `rows`.
pub fn group_parents_batch(rows: &[Row]) -> Vec<(usize, usize)> {
    let mut groups = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        if rows[i].side == 'N' {
            i += 1;
            continue;
        }
        let mut j = i;
        while j + 1 < rows.len() && rows[j + 1].ts == rows[i].ts && rows[j + 1].side == rows[i].side
        {
            j += 1;
        }
        groups.push((i, j + 1));
        i = j + 1;
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(ts: i64, side: char) -> Row {
        Row {
            ts,
            instrument_id: "X".to_string(),
            side,
            price: 0,
            size: 0,
            bid_px: 0,
            ask_px: 0,
            bid_sz: 0,
            ask_sz: 0,
            book: "normal",
        }
    }

    #[test]
    fn classify_book_matches_python_rules() {
        assert_eq!(classify_book(0, 100), "nonpositive");
        assert_eq!(classify_book(100, 0), "nonpositive");
        assert_eq!(classify_book(200, 100), "crossed");
        assert_eq!(classify_book(100, 100), "locked");
        assert_eq!(classify_book(100, 200), "normal");
    }

    #[test]
    fn group_parents_batch_splits_on_ts_side_change_and_unsided_terminates() {
        let rows = vec![
            row(1, 'B'),
            row(1, 'B'),
            row(1, 'N'),
            row(1, 'B'),
            row(2, 'A'),
        ];
        let groups = group_parents_batch(&rows);
        assert_eq!(groups, vec![(0, 2), (3, 4), (4, 5)]);
    }

    #[test]
    fn column_indices_refuses_missing_column() {
        let err = column_indices("ts_event,side,price").unwrap_err();
        assert!(matches!(err, LabError::Refusal(_)));
    }
}
