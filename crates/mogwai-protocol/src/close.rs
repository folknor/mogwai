//! Websocket close semantics, shared by the venue and by every consumer that
//! reads a close frame.
//!
//! THE CLOSE CODE DOES NOT CARRY THE SEMANTIC, and this module exists because
//! an adapter once believed it did. WS 1000 is the ordinary code for ANY
//! graceful close: the venue sends it when a run completes, when a passenger's
//! configured duration elapses, and when a newer connection evicts this one -
//! and a proxy or a load balancer closing an idle socket sends it too, on
//! behalf of nobody in this protocol. Three different terminal meanings and one
//! non-meaning share two bytes.
//!
//! So the REASON string is the discriminator, and it is a protocol contract
//! rather than a log line: the venue writes these exact strings and a consumer
//! classifies against them. The terminal FRAMES - `VenueMessage::RunComplete`
//! and `VenueMessage::PassengerDurationComplete` - remain the primary signals,
//! one per terminal this module names except eviction, which has no frame at
//! all. The close is their socket-level fallback for a reader that loses the
//! final text frame while the venue drains, and it agrees with the frame rather
//! than refining it.
//!
//! A reason this module does not recognize is NOT terminal. That is the safe
//! default in both directions: an unrecognized graceful close is a transport
//! event a consumer may redial through, where treating it as completion would
//! silently end a run that is still going.

/// WS 1000, "Normal Closure".
pub const NORMAL: u16 = 1000;

/// The most reason bytes a close frame can carry.
///
/// RFC 6455 caps a CONTROL frame's payload at 125 bytes and a close frame
/// spends the first two of them on the status code, so 123 is the whole budget
/// for the reason. This is NOT the same cap as `MAX_REASON_LEN`, which bounds a
/// reason travelling inside a TEXT frame and is four times larger; a close
/// reason trimmed only to that one is still four times too long for the frame
/// it goes in.
///
/// AN OVER-LONG REASON IS NOT A COSMETIC DEFECT HERE, which is why the cap
/// lives beside `classify` rather than at a call site. A conforming peer must
/// fail the connection on an oversized control frame, so the close that carries
/// the discriminator either never leaves or never arrives - and a consumer that
/// sees an abrupt EOF instead of a reasoned close classifies nothing, which is
/// exactly the eviction-redial loop this module exists to prevent. The venue's
/// own eviction reason reaches 135 bytes at `MAX_ACCOUNT_ID_LEN`.
pub const MAX_REASON_BYTES: usize = 123;

/// Trim a close reason to `MAX_REASON_BYTES` on a char boundary.
///
/// TRIM THE TAIL, NEVER THE HEAD, and compose the reason before calling this:
/// every discriminator this module reads is either a whole short constant or a
/// PREFIX, so a reason built prefix-first survives the trim as the same
/// terminal while the droppable detail is what gets spent. A caller that
/// appended its discriminator would classify differently after a trim than
/// before it.
#[must_use]
pub fn fit_reason(mut reason: String) -> String {
    if reason.len() <= MAX_REASON_BYTES {
        return reason;
    }
    let mut end = MAX_REASON_BYTES;
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    reason.truncate(end);
    reason
}

/// The venue's run finished - the whole run, for every passenger.
pub const RUN_COMPLETE: &str = "run complete";

/// This passenger's own configured duration elapsed. The run may continue for
/// others; for this connection it is over, and redialling would start a fresh
/// duration rather than resume anything.
///
/// The venue sends `VenueMessage::PassengerDurationComplete` ahead of this
/// close, so a consumer classifying on the frame and one classifying on the
/// close reach the SAME answer. That is what this reason could not promise
/// while both completions announced one frame: a frame-reading consumer called
/// a passenger's own deadline a finished run and never read the close, so this
/// arm was reachable only when the frame was lost, and nothing could be built on
/// it. It is now an ordinary agreeing signal.
pub const DURATION_COMPLETE: &str = "passenger duration complete";

/// Prefix on the eviction close's reason. The remainder names the account.
///
/// Terminal for this consumer, and terminal for a DIFFERENT reason than
/// completion: nothing failed and nothing finished, but a consumer that redialled
/// here would evict whatever evicted it, forever.
pub const EVICTED_PREFIX: &str = "evicted: ";

/// What a graceful close means to a consumer that must decide whether to redial.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terminal {
    /// The run is over. Stop.
    RunComplete,
    /// This passenger's duration is over. Stop.
    DurationComplete,
    /// A newer connection took this account. Stop, and do not redial.
    Evicted,
}

/// Classifies a close frame. `None` means "not a terminal this protocol
/// defines", which includes every non-1000 code and every 1000 whose reason
/// this venue did not write - redial policy for those belongs to the consumer.
#[must_use]
pub fn classify(code: u16, reason: &str) -> Option<Terminal> {
    if code != NORMAL {
        return None;
    }
    match reason {
        RUN_COMPLETE => Some(Terminal::RunComplete),
        DURATION_COMPLETE => Some(Terminal::DurationComplete),
        _ if reason.starts_with(EVICTED_PREFIX) => Some(Terminal::Evicted),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three reasons the venue writes are the ones that MUST survive
    /// intact: a trimmed constant classifies as nothing, and nothing is
    /// non-terminal. `EVICTED_PREFIX` needs room for a detail after it too,
    /// which is what makes this an inequality rather than a bare fit check.
    #[test]
    fn every_venue_reason_fits_a_close_frame_with_room_to_spare() {
        assert!(RUN_COMPLETE.len() < MAX_REASON_BYTES);
        assert!(DURATION_COMPLETE.len() < MAX_REASON_BYTES);
        assert!(EVICTED_PREFIX.len() < MAX_REASON_BYTES);
        assert_eq!(fit_reason(RUN_COMPLETE.to_owned()), RUN_COMPLETE);
        assert_eq!(fit_reason(DURATION_COMPLETE.to_owned()), DURATION_COMPLETE);
    }

    /// A trimmed reason must classify as whatever the untrimmed one did. The
    /// prefix is at the head, so it does; this pins that the trim never eats
    /// it, which is the whole reason `CloseSpec::evicted` composes before
    /// trimming.
    #[test]
    fn a_trimmed_eviction_reason_still_classifies_as_an_eviction() {
        let long = format!("{EVICTED_PREFIX}{}", "detail ".repeat(60));
        assert!(long.len() > MAX_REASON_BYTES, "the fixture must overflow");
        let fitted = fit_reason(long);
        assert_eq!(fitted.len(), MAX_REASON_BYTES);
        assert_eq!(classify(NORMAL, &fitted), Some(Terminal::Evicted));
    }

    /// A multi-byte character straddling the cut is dropped whole rather than
    /// sliced into invalid UTF-8.
    #[test]
    fn the_trim_lands_on_a_char_boundary() {
        let reason = format!("{}\u{1f600}", "y".repeat(MAX_REASON_BYTES - 1));
        assert_eq!(fit_reason(reason).len(), MAX_REASON_BYTES - 1);
    }

    #[test]
    fn an_unrecognized_normal_close_is_not_terminal() {
        assert_eq!(classify(NORMAL, ""), None);
        assert_eq!(classify(NORMAL, "idle timeout"), None);
        // A fault carries its own code and is never a terminal here.
        assert_eq!(classify(1011, RUN_COMPLETE), None);
    }

    #[test]
    fn each_venue_reason_classifies_to_its_own_terminal() {
        assert_eq!(classify(NORMAL, RUN_COMPLETE), Some(Terminal::RunComplete));
        assert_eq!(
            classify(NORMAL, DURATION_COMPLETE),
            Some(Terminal::DurationComplete)
        );
        assert_eq!(
            classify(
                NORMAL,
                &format!("{EVICTED_PREFIX}another connection claimed account A")
            ),
            Some(Terminal::Evicted)
        );
    }
}
