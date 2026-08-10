// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Stage A's zero-price, no-book session accumulator.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::{
    Block2Cell, COUNT_WINDOWS_S, NS_PER_HOUR, NS_PER_MIN, close_run,
};
use crate::error::{LabError, LabResult};
use crate::session::{SessionSegment, segment_labels, session_segment_at};

const NS_PER_SECOND: u64 = 1_000_000_000;

#[derive(Debug, Clone, Copy, Default)]
struct MinuteAcc {
    populated: bool,
    parents: u64,
}

#[derive(Debug)]
struct SegmentAcc {
    origin_ns: u64,
    end_ns: u64,
    second_counts: Vec<u64>,
}

impl SegmentAcc {
    fn new(origin_ns: u64, end_ns: u64) -> Self {
        let seconds = usize::try_from((end_ns - origin_ns) / NS_PER_SECOND)
            .expect("a session segment length fits usize");
        Self {
            origin_ns,
            end_ns,
            second_counts: vec![0; seconds],
        }
    }

    fn push_parent(&mut self, ts_ns: u64) {
        if ts_ns >= self.origin_ns && ts_ns < self.end_ns {
            let second = usize::try_from((ts_ns - self.origin_ns) / NS_PER_SECOND)
                .expect("a session second fits usize");
            self.second_counts[second] += 1;
        }
    }
}

/// The exact reduced session state needed by the Stage A verdict path.
#[derive(Debug)]
pub struct ScreenSessionAcc {
    date: String,
    pub session_start_ns: u64,
    segments: [SegmentAcc; 2],
    minutes: Vec<MinuteAcc>,
    invalid_minute: Option<u64>,
}

impl ScreenSessionAcc {
    #[must_use]
    pub fn new(date: String, seg: &SessionSegment, offset_minutes: i32) -> Self {
        let overnight = session_segment_at(seg.session_start_ns, offset_minutes)
            .expect("the open instant is inside the overnight segment");
        let post = session_segment_at(seg.session_end_ns - NS_PER_MIN, offset_minutes)
            .expect("the pre-close minute is inside the post-halt segment");
        let minute_count = usize::try_from((seg.session_end_ns - seg.session_start_ns) / NS_PER_MIN)
            .expect("a session minute count fits usize");
        Self {
            date,
            session_start_ns: seg.session_start_ns,
            segments: [
                SegmentAcc::new(overnight.segment_origin_ns, overnight.segment_end_ns),
                SegmentAcc::new(post.segment_origin_ns, post.segment_end_ns),
            ],
            minutes: vec![MinuteAcc::default(); minute_count],
            invalid_minute: None,
        }
    }

    fn segment_for_minute(&self, minute: u64) -> Option<&SegmentAcc> {
        let start_ns = minute * NS_PER_MIN;
        self.segments
            .iter()
            .find(|seg| start_ns >= seg.origin_ns && start_ns < seg.end_ns)
    }

    fn minute_mut(&mut self, minute: u64) -> Option<&mut MinuteAcc> {
        let first = self.session_start_ns / NS_PER_MIN;
        let index = usize::try_from(minute.checked_sub(first)?).ok()?;
        self.minutes.get_mut(index)
    }

    fn record_invalid_minute(&mut self, minute: u64) {
        self.invalid_minute = Some(self.invalid_minute.map_or(minute, |old| old.min(minute)));
    }

    /// Zero-price prints only establish that a minute is populated.
    pub fn push_print(&mut self, ts_ns: u64) {
        let minute = ts_ns / NS_PER_MIN;
        if self.segment_for_minute(minute).is_none() {
            self.record_invalid_minute(minute);
        } else if let Some(slot) = self.minute_mut(minute) {
            slot.populated = true;
        } else {
            self.record_invalid_minute(minute);
        }
    }

    /// One parent contributes to its minute and to the fixed second buckets.
    pub fn push_parent(&mut self, segment_index: u8, first_ts: u64) -> LabResult<()> {
        let minute = first_ts / NS_PER_MIN;
        if let Some(slot) = self.minute_mut(minute) {
            slot.parents += 1;
        } else {
            self.record_invalid_minute(minute);
        }
        let segment = self
            .segments
            .get_mut(usize::from(segment_index))
            .ok_or_else(|| LabError::refusal("screen parent has an invalid segment index"))?;
        segment.push_parent(first_ts);
        Ok(())
    }

    pub fn close(self) -> LabResult<Value> {
        Ok(json!({
            "session_date": self.date,
            "block1_hist": self.block1()?,
            "block2": self.block2(),
        }))
    }

    fn block1(&self) -> LabResult<Value> {
        if let Some(minute) = self.invalid_minute {
            return Err(LabError::refusal(format!(
                "minute {minute} carries rows but maps to no open segment of {}",
                self.date
            )));
        }
        let mut hist: BTreeMap<(u64, u64, &'static str, &'static str), u64> = BTreeMap::new();
        let first = self.session_start_ns / NS_PER_MIN;
        for (index, slot) in self.minutes.iter().enumerate() {
            if !slot.populated && slot.parents == 0 {
                continue;
            }
            let minute = first + index as u64;
            let start_ns = minute * NS_PER_MIN;
            let seg = self.segment_for_minute(minute).ok_or_else(|| {
                LabError::refusal(format!(
                    "minute {minute} carries rows but maps to no open segment of {}",
                    self.date
                ))
            })?;
            let (since, until) = segment_labels(start_ns, seg.origin_ns, seg.end_ns);
            let hour = (start_ns / NS_PER_HOUR) % 24;
            *hist.entry((slot.parents, hour, since, until)).or_insert(0) += 1;
        }
        Ok(Value::Array(
            hist.into_iter()
                .map(|((n, hour, since, until), count)| {
                    json!({
                        "n": n,
                        "quote_range_half_ticks": Value::Null,
                        "trade_range_ticks": 0,
                        "hour": hour,
                        "since_open_bin": since,
                        "until_close_bin": until,
                        "count": count,
                    })
                })
                .collect(),
        ))
    }

    fn block2(&self) -> Value {
        let mut cells: BTreeMap<(u64, u64), Block2Cell> = BTreeMap::new();
        for seg in &self.segments {
            for &window_s in COUNT_WINDOWS_S {
                #[expect(clippy::cast_sign_loss, reason = "the windows are 1, 5 and 60 seconds")]
                let window_s = window_s as u64;
                let width = usize::try_from(window_s).expect("a window width fits usize");
                let mut prev_count: Option<u64> = None;
                let mut prev_hour: Option<u64> = None;
                let mut run = 0u64;
                for (index, counts) in seg.second_counts.chunks_exact(width).enumerate() {
                    let start = seg.origin_ns
                        + u64::try_from(index * width).expect("a window offset fits u64")
                            * NS_PER_SECOND;
                    let stop = start + window_s * NS_PER_SECOND;
                    let start_hour = (start / NS_PER_HOUR) % 24;
                    let end_hour = (stop / NS_PER_HOUR) % 24;
                    if start_hour != end_hour {
                        close_run(&mut run, prev_hour, window_s, &mut cells);
                        prev_count = None;
                        prev_hour = None;
                        continue;
                    }
                    let hour = end_hour;
                    if prev_hour.is_some() && prev_hour != Some(hour) {
                        close_run(&mut run, prev_hour, window_s, &mut cells);
                        prev_count = None;
                    }
                    let count = counts.iter().sum::<u64>();
                    let cell = cells.entry((hour, window_s)).or_default();
                    cell.scheduled += 1;
                    if count == 0 {
                        cell.zeros += 1;
                    }
                    *cell.count_hist.entry(count).or_insert(0) += 1;
                    if let Some(prev) = prev_count {
                        cell.paired += 1;
                        cell.sum_x += prev;
                        cell.sum_y += count;
                        cell.sumsq_x += prev * prev;
                        cell.sumsq_y += count * count;
                        cell.sum_xy += prev * count;
                    }
                    prev_count = Some(count);
                    prev_hour = Some(hour);
                    if count > 0 {
                        run += 1;
                    } else {
                        close_run(&mut run, prev_hour, window_s, &mut cells);
                    }
                }
                close_run(&mut run, prev_hour, window_s, &mut cells);
            }
        }
        let mut out = serde_json::Map::new();
        for ((hour, window), cell) in cells {
            out.entry(hour.to_string())
                .or_insert_with(|| Value::Object(serde_json::Map::new()))
                .as_object_mut()
                .expect("an hour entry is an object")
                .insert(window.to_string(), cell.finish());
        }
        Value::Object(out)
    }
}
