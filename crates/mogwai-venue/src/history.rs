// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Paged history over a passenger's own socket.
//!
//! A history request carried here names no symbol, because the connection
//! already resolved one. That is the whole reason this exists: after the river
//! fork one label names several rivers, so a poll naming a symbol and no
//! passenger names none of them, and a passenger reading surged water would
//! backfill the clean river's prints without either side noticing. Every
//! alternative restates at the history call what the upgrade already settled,
//! which is a second place for identity to live and therefore to drift.
//!
//! What this module owns is the paging contract. The venue fixes a cutoff at
//! the first page of a session and every later page of that session uses the
//! same one, so pagination cannot chase a moving present and never finish.

use mogwai_protocol::{HistoryKind, HistoryRow};

use crate::source::{RiverKey, Rivers};

/// How many rows one page carries.
///
/// Smaller than the HTTP route's `MAX_HISTORY_LIMIT` and deliberately so. An
/// HTTP page is a whole response body that the caller waits on alone; a socket
/// page shares a connection with live market frames and execution output, and
/// the writer sends one frame at a time. A page big enough to be worth a
/// round trip but small enough that sending it does not hold the socket away
/// from the live tape for long is a different quantity from a page sized to
/// amortize an HTTP request.
pub(crate) const HISTORY_PAGE_ROWS: usize = 4_096;

/// Where a history session is up to, as handed to the consumer and back.
///
/// Opaque on the wire. The consumer echoes it and nothing more, which is what
/// lets the position stop being a timestamp the day the generator permits two
/// rows of one kind at one instant - the contract never named one.
///
/// Unauthenticated, and safe because nothing is taken on trust. A forged token
/// is not a hole here, because every field is re-checked against the connection
/// rather than believed: the river comes from the socket's own boat and is
/// never read from the token, the kind must match the request that carried it,
/// and the cutoff is re-clamped to the run present on every page. What is left
/// for a forger to move is the position, and naming a position is what `start`
/// already does on an ordinary request. Integrity protection would buy nothing
/// that re-validation does not already give.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Continuation {
    pub(crate) kind: HistoryKind,
    /// The session's immutable end, fixed at its first page.
    pub(crate) cutoff: u64,
    /// The first instant this page has not yet delivered.
    pub(crate) next_ts: u64,
}

impl Continuation {
    /// `h1:<kind>:<cutoff>:<next_ts>`, versioned so a later shape is refused
    /// rather than misread as this one.
    pub(crate) fn encode(self) -> String {
        let kind = match self.kind {
            HistoryKind::Trades => 't',
            HistoryKind::Quotes => 'q',
        };
        format!("h1:{kind}:{}:{}", self.cutoff, self.next_ts)
    }

    pub(crate) fn decode(token: &str) -> Option<Self> {
        let mut parts = token.split(':');
        if parts.next()? != "h1" {
            return None;
        }
        let kind = match parts.next()? {
            "t" => HistoryKind::Trades,
            "q" => HistoryKind::Quotes,
            _ => return None,
        };
        let cutoff = parts.next()?.parse().ok()?;
        let next_ts = parts.next()?.parse().ok()?;
        // A trailing field means a token this version does not understand, and
        // guessing at it is how a shape change becomes a silent misread.
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            kind,
            cutoff,
            next_ts,
        })
    }
}

/// One page, plus what the consumer needs to ask for the next.
pub(crate) struct Page {
    pub(crate) rows: Vec<HistoryRow>,
    pub(crate) cutoff: u64,
    pub(crate) continuation: Option<String>,
    pub(crate) complete: bool,
}

/// Why a page will not be produced. Distinct from an empty page on purpose:
/// representing unavailable history as a quiet market is the defect this whole
/// shape exists to avoid.
pub(crate) struct Refusal {
    pub(crate) reason: String,
    pub(crate) retryable: bool,
}

impl Refusal {
    fn materialization(error: &crate::source::MaterializeRefusal) -> Self {
        Self {
            reason: format!("history could not be produced: {error:#}"),
            retryable: error.is_venue_fault(),
        }
    }
}

/// What one page asks for. The river comes from the caller's own boat, which is
/// why there is no symbol here to get wrong.
pub(crate) struct PageRequest<'a> {
    pub(crate) key: &'a RiverKey,
    pub(crate) kind: HistoryKind,
    pub(crate) start: Option<u64>,
    pub(crate) end: Option<u64>,
    pub(crate) continuation: Option<&'a str>,
    /// The asking passenger's present, snapshotted once by the caller. Taking
    /// it here instead would let a single request be answered against a moving
    /// clock.
    ///
    /// The tighter of two bounds, and the second one is the point. The run
    /// clock keeps any caller from reading past the venue's present, which is
    /// why a forward claim from this venue is worth something. But a passenger
    /// on a slow boat is behind the run clock, and serving it rows between its
    /// own instant and the run's would hand it its own future - the look-ahead
    /// the run bound exists to prevent, arriving one level down. A history read
    /// over a socket is a passenger asking about its own ride, so it is bounded
    /// by that ride.
    pub(crate) present: u64,
    pub(crate) run_start_ns: u64,
}

/// Synthesize one page of this passenger's own river.
///
/// Runs the generator walk, so it belongs on a blocking thread: `next_tick`
/// never blocks on IO but never ends either, and a page grinds until it fills
/// or crosses its cutoff.
pub(crate) fn serve_page(rivers: &Rivers, request: &PageRequest<'_>) -> Result<Page, Refusal> {
    let PageRequest {
        key,
        kind,
        start,
        end,
        continuation,
        present,
        run_start_ns,
    } = *request;
    let resumed = match continuation {
        Some(token) => Some(Continuation::decode(token).ok_or_else(|| Refusal {
            reason: "unreadable continuation; start the history session again".to_owned(),
            retryable: false,
        })?),
        None => None,
    };
    if let Some(resumed) = resumed
        && resumed.kind != kind
    {
        // The token carries the kind so that resuming a trade session under a
        // quote request is refused rather than answered from the wrong stream.
        // The two are separate streams with separate cutoffs, so silently
        // honoring the request's kind would hand back rows the session's own
        // cutoff was never fixed against.
        return Err(Refusal {
            reason: "continuation belongs to the other history stream".to_owned(),
            retryable: false,
        });
    }

    // Fixed once per session, then carried. A cutoff recomputed per page would
    // move with the present, so a consumer paginating a busy river could never
    // reach the end - each page would push the finish line out. Re-clamped
    // rather than trusted, so a fabricated token cannot authorize reading past
    // the passenger's own present: this venue exists so a forward claim is
    // worth something, and a run that read its own future looks clean and is
    // not.
    let cutoff = resumed
        .map_or(end.unwrap_or(present), |resumed| resumed.cutoff)
        .min(present);
    let from = resumed.map_or(start, |resumed| Some(resumed.next_ts));

    if let Some(from) = from
        && from > cutoff
    {
        // Not an error: a session resumed exactly at its cutoff has delivered
        // everything it promised.
        return Ok(Page {
            rows: Vec::new(),
            cutoff,
            continuation: None,
            complete: true,
        });
    }

    // Every river owes the whole warmup span before it can be served, and a
    // passenger's first history request is a first requester like any other.
    // Idempotent: a river already past the run origin walks nothing.
    // Keeping the refusal typed through this boundary makes a permanent
    // request or capacity refusal non-retryable, while a failure by a river the
    // venue already promised remains retryable.
    rivers
        .ensure_reach(key, run_start_ns)
        .map_err(|error| Refusal::materialization(&error))?;

    let rows = match kind {
        HistoryKind::Trades => {
            crate::http::bounded_trades(key, from, Some(cutoff), HISTORY_PAGE_ROWS, rivers)
                .map(|rows| rows.into_iter().map(HistoryRow::Trade).collect::<Vec<_>>())
        }
        HistoryKind::Quotes => {
            crate::http::bounded_quotes(key, from, Some(cutoff), HISTORY_PAGE_ROWS, rivers)
                .map(|rows| rows.into_iter().map(HistoryRow::Quote).collect::<Vec<_>>())
        }
    }
    .map_err(|error| Refusal {
        reason: format!("history could not be produced: {error:#}"),
        retryable: true,
    })?;

    // A short page is the end of the window, a full one is not. The walk stops
    // for exactly two reasons - it filled the page, or it crossed the cutoff -
    // so a page below the ceiling can only be the second.
    let complete = rows.len() < HISTORY_PAGE_ROWS;
    let continuation = if complete {
        None
    } else {
        // Resume strictly after the last row delivered, which is sound because
        // a river never prints two rows of one kind at one instant - gated by
        // `a_river_never_prints_two_trades_at_one_instant` and
        // `a_river_never_prints_two_quotes_at_one_instant` in `mogwai-data`,
        // and re-asserted through this crate's own served source by
        // `a_served_history_source_prints_at_most_one_row_of_one_kind_per_instant`
        // below, because a premise this module's correctness rests on owes a
        // gate in the crate that rests on it.
        // Resuming at the last row would re-deliver it on every page; resuming
        // after an instant that could hold a second row would lose it.
        let last = rows.last().map_or(cutoff, HistoryRow::ts_event);
        Some(
            Continuation {
                kind,
                cutoff,
                next_ts: last.saturating_add(1),
            }
            .encode(),
        )
    };

    Ok(Page {
        rows,
        cutoff,
        continuation,
        complete,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn materialization_retryability_follows_the_typed_classification() {
        let cap = Refusal::materialization(&crate::source::MaterializeRefusal::CapacityExhausted {
            cap: 256,
            count: 256,
        });
        assert!(!cap.retryable, "a spent river cap cannot change on retry");

        let reach = Refusal::materialization(&crate::source::MaterializeRefusal::Reach(
            anyhow::anyhow!("walk stopped"),
        ));
        assert!(reach.retryable, "a synthesis failure belongs to the venue");
    }

    #[test]
    fn a_continuation_round_trips_through_its_token() {
        for kind in [HistoryKind::Trades, HistoryKind::Quotes] {
            let token = Continuation {
                kind,
                cutoff: 9_876_543_210,
                next_ts: 1_234_567_890,
            };
            assert_eq!(
                Continuation::decode(&token.encode()),
                Some(token),
                "{kind:?} must survive its own encoding"
            );
            assert!(
                token.encode().len() <= mogwai_protocol::MAX_CONTINUATION_LEN,
                "the token the venue emits must fit the bound it enforces"
            );
        }
    }

    /// The premise the continuation rests on, re-asserted where it is relied
    /// on.
    ///
    /// `serve_page` resumes strictly after the last row's instant, which loses
    /// a row unless a river prints at most one row of one kind per instant.
    /// That invariant is `mogwai-data`'s, and its own tests
    /// (`a_river_never_prints_two_trades_at_one_instant` and the quote twin)
    /// are where it is established. This crate owes a copy anyway, for the same
    /// reason the adapter's `owns_a_fresh_exec_sink_on_every_lane` asserts a
    /// premise it merely depends on: the paging code here is correct only if
    /// the invariant holds through the source this crate actually consumes -
    /// the checkpointed, merged history source, not the generator in
    /// isolation - and if it ever stopped holding here, nothing in this crate
    /// would say so. The continuation would just skip rows, silently, and a
    /// consumer would read a thinner market than the venue printed.
    ///
    /// Bounded on purpose: a fixed row count off one river, no sockets, no run.
    #[test]
    fn a_served_history_source_prints_at_most_one_row_of_one_kind_per_instant() {
        // Enough rows to cross many arrival draws and several page-sized
        // stretches, and small enough to stay a unit test.
        const SPAN_ROWS: usize = 2_048;

        let rivers = crate::fills::test_rivers();
        let key = rivers.test_key("BTCUSDT");

        let trades = crate::http::bounded_trades(&key, Some(0), None, SPAN_ROWS, &rivers)
            .expect("a trade span");
        assert_eq!(trades.len(), SPAN_ROWS, "the span must actually be walked");
        for pair in trades.windows(2) {
            assert!(
                pair[0].ts_event < pair[1].ts_event,
                "two trades at ts_event={}, so a continuation resuming after it would lose one",
                pair[0].ts_event
            );
        }

        let quotes = crate::http::bounded_quotes(&key, Some(0), None, SPAN_ROWS, &rivers)
            .expect("a quote span");
        assert_eq!(quotes.len(), SPAN_ROWS, "the span must actually be walked");
        for pair in quotes.windows(2) {
            assert!(
                pair[0].ts_event < pair[1].ts_event,
                "two quotes at ts_event={}, so a continuation resuming after it would lose one",
                pair[0].ts_event
            );
        }
    }

    /// A token this version does not understand is refused rather than read as
    /// far as it parses. Guessing at an unknown shape is how a later format
    /// becomes a silent misread of a cutoff or a position.
    #[test]
    fn an_unreadable_continuation_is_refused_rather_than_partially_read() {
        for bad in [
            "",
            "h2:t:1:2",
            "h1:x:1:2",
            "h1:t:1",
            "h1:t:1:2:3",
            "h1:t:notanumber:2",
            "garbage",
        ] {
            assert_eq!(
                Continuation::decode(bad),
                None,
                "{bad:?} must not decode into a position"
            );
        }
    }
}
