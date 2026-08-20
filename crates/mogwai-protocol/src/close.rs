//! Websocket close semantics, shared by the venue and by every client that
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
//! rather than a log line: the venue writes these exact strings and a client
//! classifies against them. `ServerMessage::RunComplete` remains the primary
//! completion signal; the close is its socket-level fallback for a reader that
//! loses the final text frame while the server drains.
//!
//! A reason this module does not recognize is NOT terminal. That is the safe
//! default in both directions: an unrecognized graceful close is a transport
//! event a client may redial through, where treating it as completion would
//! silently end a run that is still going.

/// WS 1000, "Normal Closure".
pub const NORMAL: u16 = 1000;

/// The venue's run finished - the whole run, for every passenger.
pub const RUN_COMPLETE: &str = "run complete";

/// This passenger's own configured duration elapsed. The run may continue for
/// others; for this connection it is over, and redialling would start a fresh
/// duration rather than resume anything.
///
/// THE VENUE SENDS A `RunComplete` TEXT FRAME AHEAD OF THIS CLOSE, the same as
/// it does for a genuinely finished run, so a client that classifies on the
/// text frame will call this a run completion and never look at the close. This
/// reason is therefore a REFINEMENT available to a client that reads the close,
/// not a signal it is guaranteed to act on. Both readings stop the client,
/// which is why the imprecision is tolerable; nothing may be built on this arm
/// being reached.
pub const DURATION_COMPLETE: &str = "passenger duration complete";

/// Prefix on the eviction close's reason. The remainder names the account.
///
/// Terminal for this client, and terminal for a DIFFERENT reason than
/// completion: nothing failed and nothing finished, but a client that redialled
/// here would evict whatever evicted it, forever.
pub const EVICTED_PREFIX: &str = "evicted: ";

/// What a graceful close means to a client that must decide whether to redial.
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
/// this venue did not write - redial policy for those belongs to the client.
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
